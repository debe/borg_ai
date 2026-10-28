//! Two members of one collective reconcile its logs over iroh (QUIC, TLS with raw ed25519 keys).
//!
//! iroh authenticates the peer's key during the handshake; a node then checks that key against
//! its verified membership before it sends a single byte of the collective. One session is a
//! sequence of swaps on one stream (the dialler speaks first, the listener answers after it has
//! checked what it heard). The order is the one PLAN.md "Reconnect" fixes, trust first:
//!
//! 1. `Hello`: which collective (the listener refuses a name that is not its own), and the port the
//!    dialler listens on. The listener remembers it in peers.json at the IP QUIC saw the dialler
//!    come from, so a node bound to 0.0.0.0 is still dialled back at an address that works.
//! 2. `Membership`: both nodes' whole history of signed versions; each resolves it from its own
//!    root (membership.rs: every step signed by an admin of the previous one, forks resolved the
//!    same way everywhere). The peer must still be a member under the result, and revocations are
//!    applied (quarantine) before any data moves, also when the held version did not change but
//!    the logs were never checked against it (`origin::apply_if_new`).
//! 3. `lessons.jsonl`, `stats.jsonl`, `outcomes-team.jsonl`, `playbooks.jsonl`, `constraints.jsonl` (lines.rs `SYNCED`), each in turn:
//!    range-based set reconciliation (recon.rs), cost ∝ difference: one fingerprint when nothing
//!    changed. Then `Lines`: what the other side lacks, each line with its
//!    signed origin (origin.rs). A line is stored only if the peer announced it, its origin verifies
//!    and the membership admits its origin key at its stamp time.
//!
//!    3b. `Goals`: the removed hive's step, still spoken so a 0.3.0 peer keeps syncing on
//!    `borg/mesh/12`. This node names no goal; for each valid goal id the peer names it follows the
//!    peer's reconciliation of that goal's `LEGACY_GOAL_LOGS` logs and stores nothing (`skip`). What
//!    a peer gave is remembered in memory (`Node::legacy`), so it is not sent again every session.
//!
//! One `Lines` frame carries at most `MAX_GIVE` bytes of lines; the rest go at the next session (the
//! sets still differ, so its reconciliation finds them again).
//! 4. `reasoning.jsonl` (git-note excerpts, notes in the borg crate): not reconciled with everyone.
//!    Each author offers its own excerpts to the k = 2 members that rendezvous hashing assigns to
//!    each excerpt (`replicas`), so the reasoning survives the author being offline without every
//!    member holding all of it.
//! 5. `Ask`: the commits whose reasoning a member wants (`borg notes fetch`); any node that holds
//!    an admitted excerpt for one (or a link record with it on either side) answers with it, author
//!    or replica, so the "why" can be read back while its author is offline.
//! 6. `Live`: ephemeral, never reconciled into a log: presence (live.rs: which drones run where,
//!    latest per member and session, a day at most) and signed steering directives (steer.rs:
//!    checked against the membership on receipt; refused ones are dropped and logged). A 0.3.0
//!    peer's hive beats in it are ignored.
//!
//! A completed session, dialled or served, is recorded on each side by when it started, per member
//! (`mesh/synced.json`, borg presence.rs `synced_path`).
//!
//! Addressing is by key (addr.rs): a peer is dialled at its node id, with direct addresses and
//! relay URLs as optional hints. How the endpoint reaches it across NAT is the collective's relay
//! setting: n0's relays and address lookup (default), a self-hosted relay, or direct only.
//!
//! **Joining** (invite.rs) is a second ALPN: a node that is not a member yet presents a signed
//! invite to the admin that issued it, and gets the membership history back (`join`, `admit`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use borg::collective::{self, Config, Route};
use iroh::Endpoint;
use iroh::endpoint::{Builder, ConnectOptions, Connection, Incoming, IncomingAddr, PortmapperConfig, RecvStream, SendStream, presets};
use iroh::tls::CaTlsConfig;
use serde::{Deserialize, Serialize};

use crate::addr::{self, Hint, Relay, Target};
use crate::identity::Identity;
use crate::invite::{self, Invite, Ticket};
use crate::lines::{self, Hash, REASONING, SYNCED, logs};
use crate::membership::Membership;
use crate::origin::{self, Origin};
use crate::recon::{self, Recon};
use crate::{from_hex, peers, to_hex};
use crate::{live, steer};

mod vectors;

/// The session protocol. 12 is kept since the hive's removal: its steps (`Goals`, the beats of `Live`)
/// are still spoken empty, so a 0.3.0 peer syncs the lessons with this node. 11: `constraints.jsonl` is
/// a fifth synced log (lines.rs `SYNCED`); the logs reconcile in a fixed list, so a peer on 10 would
/// reconcile another file at the same step and is refused at the handshake instead.
pub const ALPN: &[u8] = b"borg/mesh/12";
/// The protocols of older nodes (`borg/mesh/11` down to `/1`), offered and accepted beside `ALPN` only to
/// tell a mismatch: without them the handshake fails and nothing says why. A session on one is closed at
/// once and the peer recorded (`mismatch`). A newer node does the same with this one's `ALPN`.
fn older() -> Vec<Vec<u8>> {
    let ours = version(ALPN).unwrap_or(0);
    (1..ours).rev().map(|v| format!("borg/mesh/{v}").into_bytes()).collect()
}

/// The number of a session protocol (`borg/mesh/<n>`).
fn version(alpn: &[u8]) -> Option<u32> {
    std::str::from_utf8(alpn).ok()?.strip_prefix("borg/mesh/")?.parse().ok()
}
/// The join handshake: an invite for the membership history (`Node::join`, `Node::admit`).
pub const JOIN_ALPN: &[u8] = b"borg/join/1";
/// Largest frame accepted; a lessons log is far smaller, a bigger frame is a broken or hostile peer.
const MAX_FRAME: usize = 256 << 20;
/// Most bytes of lines one `Lines` frame gives (well within `MAX_FRAME`): a big difference goes over
/// several sessions, never in a frame the peer refuses.
const MAX_GIVE: usize = 64 << 20;
/// How many members hold a replica of each reasoning excerpt besides its author.
pub const REPLICAS: usize = 2;
/// Most commits one `Ask` names, and most excerpts one answer carries.
const MAX_ASK: usize = 1000;
/// How many logs per goal a 0.3.0 peer reconciles in step 3b (its hive's synced logs: goal, tasks,
/// contracts, messages, flags, bundles, spend).
const LEGACY_GOAL_LOGS: usize = 7;

/// Where a node finds its collective. Kept instead of a fixed `Route` so every session re-reads the
/// config: setting the collective to `share: local`, adding the repo to `off` or breaking the file
/// stops a running `serve` at its next session, not at its next restart.
#[derive(Debug, Clone)]
pub struct Place {
    /// The collectives config (`BORG_CONFIG`).
    pub config: PathBuf,
    /// The root store (`BORG_HOME`).
    pub base: PathBuf,
    /// The directory whose collective this is (`--dir`).
    pub dir: String,
    /// The collective named directly (`--collective`, and `join` before any repo routes to it):
    /// it must be listed in the config, and its `share` still decides.
    pub collective: Option<String>,
}

impl Place {
    pub fn from_env(dir: &str) -> Place {
        Place { config: collective::config_path(), base: borg::learn::home(), dir: dir.into(), collective: None }
    }

    /// The same machine, the collective named `name` instead of the one a directory routes to.
    pub fn named(&self, name: &str) -> Place {
        Place { collective: Some(name.into()), ..self.clone() }
    }

    /// The route as of now; a broken config never leaks a repo into a collective, it routes off.
    pub fn route(&self) -> Route {
        self.resolve().1
    }

    fn resolve(&self) -> (Option<Config>, Route) {
        match collective::load_from(&self.config) {
            Ok(cfg) => {
                let r = match &self.collective {
                    None => collective::route(cfg.as_ref(), &self.dir, &self.base),
                    Some(name) => match cfg.as_ref() {
                        None if name == "default" => collective::route(None, &self.dir, &self.base),
                        None => Route::Off,
                        Some(c) => c.collectives.iter().find(|e| e.name == *name).map_or(Route::Off, |e| Route::Collective {
                            name: e.name.clone(),
                            home: collective::store(&self.base, &e.name),
                            share: e.share,
                        }),
                    },
                };
                (cfg, r)
            }
            Err(_) => (None, Route::Off),
        }
    }

    /// How this collective reaches its members (addr.rs): `BORG_RELAY`, else its `relay` setting.
    pub fn relay(&self) -> Result<Relay, String> {
        let (cfg, route) = self.resolve();
        let configured = match (&cfg, &route) {
            (Some(c), Route::Collective { name, .. }) => c.collectives.iter().find(|e| e.name == *name).and_then(|e| e.relay.clone()),
            _ => None,
        };
        Relay::choose(configured.as_deref())
    }

    /// The gate on the config as it is now: the collective's name and store, or why nothing may sync.
    pub fn gate(&self) -> Result<(String, PathBuf), String> {
        let (cfg, route) = self.resolve();
        if let (Some(name), Route::Off) = (&self.collective, &route) {
            return Err(format!("collective {name:?} is not in {}", self.config.display()));
        }
        let (name, home) = gate(&route)?;
        if let Some(local) = collective::local_owner_of(cfg.as_ref(), home, &self.base) {
            return Err(format!("store {} also belongs to local collective {local:?}: refusing to sync", home.display()));
        }
        Ok((name.to_string(), home.to_path_buf()))
    }
}

/// The gate. A collective's data may leave this machine only if `collective::may_sync` says so.
pub fn gate(route: &Route) -> Result<(&str, &Path), String> {
    match route {
        Route::Collective { name, home, .. } if collective::may_sync(route) => Ok((name, home)),
        Route::Collective { name, .. } => Err(format!("collective {name:?} is local: it never leaves this machine, refusing to sync")),
        Route::Off => Err("this repo belongs to no collective (off): nothing may sync".into()),
    }
}

/// What one session moved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Lines this node sent / newly stored (lessons and reasoning).
    pub sent: usize,
    pub received: usize,
    /// Lines the peer sent that were refused (bad origin, or an origin the membership no longer admits).
    pub rejected: usize,
    /// Lines moved to quarantine because a newer membership revoked their origin.
    pub quarantined: usize,
    /// Reconciliation of lessons: rounds, fingerprints and hashes this node sent.
    pub rounds: usize,
    pub fps: usize,
    pub hashes: usize,
    /// Reasoning excerpts this node handed to the peer as one of their replicas.
    pub replicated: usize,
    /// Reasoning excerpts this node answered an `Ask` with.
    pub answered: usize,
    /// Remote presence records that were new or newer (live.rs).
    pub presence: usize,
    /// Steering directives newly filed / refused (steer.rs).
    pub steered: usize,
    pub dropped: usize,
    /// Members this node admitted with an invite (`admit`).
    pub joined: usize,
}

/// A line and where it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signed {
    pub line: String,
    pub origin: Origin,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Msg {
    /// `relay`: the dialler's home relay, so the listener can dial it back across NAT (absent from
    /// older nodes).
    Hello {
        collective: String,
        listen: Option<String>,
        #[serde(default)]
        relay: Option<String>,
    },
    Refused {
        reason: String,
    },
    Join {
        invite: Invite,
        listen: Option<String>,
        relay: Option<String>,
    },
    Welcome {
        docs: Vec<Membership>,
    },
    Membership {
        docs: Vec<Membership>,
    },
    Round {
        round: recon::Round,
    },
    Lines {
        lines: Vec<Signed>,
    },
    /// The hive goals a side holds (3b): none from this node, a 0.3.0 peer's are skipped.
    Goals {
        ids: Vec<String>,
    },
    Offer {
        hashes: Vec<String>,
    },
    Want {
        hashes: Vec<String>,
    },
    Ask {
        commits: Vec<String>,
    },
    Live {
        presence: Vec<borg::presence::Remote>,
        directives: Vec<borg::steer::Directive>,
    },
}

/// Which CAs vouch for a relay's, address lookup's or DNS-over-HTTPS server's certificate: the
/// system store, so a company proxy that re-signs TLS with its own CA (installed on the machine)
/// does not cut the node off its relay. A machine with no readable store falls back to Mozilla's.
/// Peer connections never depend on this: iroh checks those by key.
fn ca_trust() -> CaTlsConfig {
    let system = CaTlsConfig::system();
    match system.server_cert_verifier(iroh::tls::default_provider()) {
        Ok(_) => system,
        Err(_) => CaTlsConfig::embedded(),
    }
}

/// One node of one collective: its key, its store and an iroh endpoint.
pub struct Node {
    place: Place,
    /// The store the node was bound to; a session whose route now names another store is refused.
    home: PathBuf,
    me: Identity,
    endpoint: Endpoint,
    /// Serialises this node's writes to its store (sessions run concurrently under `serve`).
    store: Mutex<()>,
    /// Whether peers are told where this node listens (false for a one-shot dial).
    announce: bool,
    /// How peers are reached (addr.rs), fixed when the endpoint was built.
    relay: Relay,
    /// The hashes each 0.3.0 peer gave per hive goal log (3b), by (peer, goal, log): never stored, so
    /// the next dial reconciles against them and the peer gives only what is new.
    legacy: Mutex<HashMap<(String, String, usize), BTreeSet<Hash>>>,
}

impl Node {
    /// Opens the endpoint for `place` on `addr` — only after the gate, so a local collective or an
    /// off repo never even gets a socket.
    pub async fn bind(place: &Place, addr: SocketAddr) -> Result<Node, String> {
        Node::open(place, Some(addr), true, |b| b).await
    }

    /// A node of a collective whose membership this machine does not hold yet: it can only `join`.
    pub async fn joining(place: &Place, addr: SocketAddr) -> Result<Node, String> {
        Node::open(place, Some(addr), false, |b| b).await
    }

    /// The endpoint for the collective's relay setting (addr.rs). `addr` is the UDP socket for
    /// direct paths (`None`: relay only, for tests); `member`: refuse without a membership;
    /// `tweak` adjusts the builder (tests trust a local relay's self-signed certificate).
    pub async fn open(place: &Place, addr: Option<SocketAddr>, member: bool, tweak: impl FnOnce(Builder) -> Builder) -> Result<Node, String> {
        let (_, home) = place.gate()?;
        let relay = place.relay()?;
        let me = Identity::load_or_create(&home)?;
        if member {
            members(&home)?;
        }
        let builder = match &relay {
            // n0's relays and DNS address lookup (pkarr): found by key alone, hole-punched across NAT.
            Relay::Default => Endpoint::builder(presets::N0),
            // A self-hosted relay and no public lookup: nothing about this node reaches n0.
            Relay::Custom(_) => Endpoint::builder(presets::Minimal).relay_mode(relay.mode()),
            // Direct addresses only: no relay, no lookup, no probing of the LAN's router.
            Relay::Disabled => Endpoint::builder(presets::Minimal).relay_mode(relay.mode()).portmapper_config(PortmapperConfig::Disabled),
        };
        let mut builder = builder
            .secret_key(me.iroh_secret())
            .alpns([vec![ALPN.to_vec(), JOIN_ALPN.to_vec()], older()].concat())
            .clear_ip_transports()
            .ca_tls_config(ca_trust());
        if let Some(a) = addr {
            builder = builder.bind_addr(a).map_err(|e| e.to_string())?;
        }
        let endpoint = tweak(builder).bind().await.map_err(|e| format!("bind {}: {e}", addr.map(|a| a.to_string()).unwrap_or("relay".into())))?;
        Ok(Node { place: place.clone(), home, me, endpoint, store: Mutex::new(()), announce: true, relay, legacy: Mutex::default() })
    }

    /// Waits until the endpoint holds a connection to its home relay (at most `within`); true if
    /// it does. Nothing to wait for with relays disabled.
    pub async fn online(&self, within: Duration) -> bool {
        self.relay == Relay::Disabled || tokio::time::timeout(within, self.endpoint.online()).await.is_ok()
    }

    /// The relay this node is reachable through, once it is online.
    pub fn home_relay(&self) -> Option<String> {
        self.endpoint.addr().relay_urls().next().map(|u| u.to_string())
    }

    /// Where another machine could reach this node: its direct addresses (`tickets`) and its home
    /// relay. What `serve` writes to `mesh/addr.json` for `invite` to put in a ticket.
    pub fn hints(&self) -> Vec<Hint> {
        let mut h: Vec<Hint> = self.tickets().iter().filter_map(|t| Target::parse(t).ok()).flat_map(|t| t.hints).collect();
        h.extend(self.home_relay().and_then(|u| addr::hints(&u).ok()).into_iter().flatten());
        h
    }

    pub fn relay(&self) -> &Relay {
        &self.relay
    }

    /// A node that only dials and exits (`sync`, `fetch`): it tells no peer where it listens,
    /// because its ephemeral port is gone a moment later.
    pub fn quiet(mut self) -> Node {
        self.announce = false;
        self
    }

    /// The gate again, on the config as it is now, and the store must still be the bound one.
    pub fn gate(&self) -> Result<(String, PathBuf), String> {
        let (name, home) = self.place.gate()?;
        if home != self.home {
            return Err(format!("{} now routes to store {}, this node serves {}: restart it", self.place.dir, home.display(), self.home.display()));
        }
        Ok((name, home))
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn public(&self) -> String {
        self.me.public()
    }

    fn addr(&self) -> Option<SocketAddr> {
        self.endpoint.bound_sockets().first().copied()
    }

    /// `<key>@<ip:port>`: what a peer passes to `sync`. The first of `tickets`.
    pub fn ticket(&self) -> String {
        self.tickets().into_iter().next().unwrap_or_else(|| format!("{}@{}", self.me.public(), self.addr().map(|a| a.to_string()).unwrap_or_default()))
    }

    /// One ticket per address another machine could dial. Bound to a specific IP that is the one;
    /// bound to an unspecified IP (0.0.0.0, the default) it is each interface address iroh found,
    /// never 0.0.0.0 itself (empty if there is none: then `--bind IP:PORT`).
    pub fn tickets(&self) -> Vec<String> {
        let Some(bound) = self.addr() else { return Vec::new() };
        let addrs: Vec<SocketAddr> = if bound.ip().is_unspecified() {
            let mut v: Vec<SocketAddr> = self
                .endpoint
                .addr()
                .ip_addrs()
                .filter(|a| !a.ip().is_unspecified() && a.is_ipv4() == bound.is_ipv4())
                .map(|a| SocketAddr::new(a.ip(), bound.port()))
                .collect();
            v.sort_by_key(|a| (a.ip().is_loopback(), *a));
            v.dedup();
            v
        } else {
            vec![bound]
        };
        addrs.into_iter().map(|a| format!("{}@{a}", self.me.public())).collect()
    }

    /// What this node announces in its hello: the address it listens on (the port is what counts).
    fn listen(&self) -> Option<String> {
        self.announce.then(|| self.addr()).flatten().map(|a| a.to_string())
    }

    /// Applies the held membership to the logs unless they were already checked against it (a
    /// `join` or `revoke` by another process, or a sync, changed it). Returns how many lines moved.
    pub fn enforce(&self) -> Result<usize, String> {
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let m = members(&self.home)?;
        steer::enforce(&self.home, &m, borg::learn::now_ms());
        origin::apply_if_new(&self.home, &m)
    }

    /// Stamps the lines this node's hooks wrote since the last call as its own (origin.rs).
    pub fn stamp(&self) -> Result<usize, String> {
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        // Fresh outcomes first, so they are stamped in the same step.
        borg::outcome::publish(&self.home);
        origin::stamp(&self.home, &self.me, &logs(&self.home), borg::learn::now_ms())
    }

    /// Records a completed session with `remote` that started at `started` (borg presence.rs
    /// `synced_path`): a claim written before it reached the peer, and the peer's earlier claims came back.
    fn synced(&self, remote: &str, started: i64) {
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let _ = borg::presence::note_synced(&self.home, remote, started);
        let _ = borg::presence::forget_mismatch(&self.home, remote);
    }

    /// A session with member `remote` that came up on another protocol (`older`): recorded for `borg
    /// doctor` (borg presence.rs `protocols_path`), closed, and the error says who has to upgrade; the
    /// close reason says the same from the peer's side.
    fn mismatch(&self, conn: &Connection, remote: &str) -> String {
        let m = borg::presence::Mismatch {
            theirs: String::from_utf8_lossy(conn.alpn()).into_owned(),
            ours: String::from_utf8_lossy(ALPN).into_owned(),
            ms: borg::learn::now_ms(),
        };
        let (line, told) = (m.line(remote), m.told(&self.me.public()));
        {
            let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let _ = borg::presence::note_mismatch(&self.home, remote, m);
        }
        conn.close(2u32.into(), told.as_bytes());
        line
    }

    /// Forgets `remote`'s last completed session (presence.rs `forget_synced`): a dial to it failed.
    pub(crate) fn unreachable(&self, remote: &str) {
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let _ = borg::presence::forget_synced(&self.home, remote);
    }

    /// The home relay this node tells peers about (not for a one-shot dial, nor with relays off).
    fn announced(&self) -> Option<String> {
        self.announce.then(|| self.home_relay()).flatten()
    }

    /// Dials the member named by `target` (`<key>[@<hints>]`, addr.rs) and reconciles.
    pub async fn sync(&self, target: &str) -> Result<Report, String> {
        self.fetch(target, &[]).await
    }

    /// `sync`, also asking the peer for the reasoning excerpts of `commits` (step 5): what it
    /// holds lands in this node's reasoning.jsonl.
    pub async fn fetch(&self, target: &str, commits: &[String]) -> Result<Report, String> {
        let (name, home) = self.gate()?;
        let (name, home) = (name.as_str(), home.as_path());
        let membership = members(home)?;
        let t = Target::parse(target)?;
        if membership.role_of(&t.key).is_none() {
            return Err(format!("{} is not a member of {name}: refusing to connect", t.key));
        }
        let started = borg::learn::now_ms();
        let connecting = self.endpoint.connect_with_opts(t.endpoint_addr(&self.relay)?, ALPN, ConnectOptions::new().with_additional_alpns(older()));
        let conn = connecting.await.map_err(|e| format!("connect {target}: {e}"))?.await.map_err(|e| format!("connect {target}: {e}"))?;
        if conn.alpn() != ALPN {
            return Err(self.mismatch(&conn, &t.key));
        }
        let result = async {
            let remote = authorise(&conn, &membership, name)?;
            let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
            let hello = Msg::Hello { collective: name.into(), listen: self.listen(), relay: self.announced() };
            let Msg::Hello { collective, .. } = swap(true, &mut tx, &mut rx, hello).await? else {
                return Err("protocol: expected hello".into());
            };
            if collective != name {
                return Err(format!("peer answered for collective {collective:?}, not {name:?}"));
            }
            let report = self.exchange(true, &mut tx, &mut rx, membership, &remote, commits).await?;
            let _ = tx.finish();
            self.synced(&remote, started);
            Ok(report)
        }
        .await;
        conn.close(0u32.into(), if result.is_ok() { b"done" } else { b"error" });
        vectors::reindexed(&self.home, result).await
    }

    /// Joins with an invite ticket (invite.rs): dials the admin that issued it, presents the signed
    /// invite, installs the membership history it answers with (trusted because iroh authenticated
    /// the ticket's admin key), remembers the admin as a peer and syncs with it at once.
    pub async fn join(&self, ticket: &Ticket) -> Result<(Membership, Report), String> {
        let (name, home) = self.gate()?;
        let (name, home) = (name.as_str(), home.as_path());
        if ticket.invite.collective != name {
            return Err(format!("the ticket is for collective {:?}, this node is {name:?}", ticket.invite.collective));
        }
        let admin = ticket.target();
        let conn = self.endpoint.connect(admin.endpoint_addr(&self.relay)?, JOIN_ALPN).await.map_err(|e| format!("connect {}: {e}", admin.key))?;
        let me = self.public();
        let result = async {
            let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
            let ask = Msg::Join { invite: ticket.invite.clone(), listen: self.listen(), relay: self.announced() };
            let Msg::Welcome { docs } = swap(true, &mut tx, &mut rx, ask).await? else { return Err("protocol: expected welcome".into()) };
            let _ = tx.finish();
            let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let installed = match origin::join(home, name, &docs) {
                Ok((m, _)) => m,
                // Joined before (a lost answer, a second ticket): the held version already lists us.
                Err(e) => Membership::load(home)?.filter(|m| m.role_of(&me).is_some()).ok_or(e)?,
            };
            if installed.role_of(&me).is_none() {
                return Err(format!("the admin's membership of {name} does not list this node ({me})"));
            }
            steer::enforce(home, &installed, borg::learn::now_ms());
            Ok(installed)
        }
        .await;
        conn.close(0u32.into(), if result.is_ok() { b"done" } else { b"error" });
        let installed = result?;
        peers::remember(home, &admin.key, &addr::join_hints(&admin.hints))?;
        let report = self.sync(&admin.to_string()).await.map_err(|e| format!("joined {name}, but the first sync failed: {e}"))?;
        Ok((installed, report))
    }

    /// Answers a node presenting an invite (`join`): checks it (invite.rs `redeem`: issued by this
    /// node, signed, not expired, not spent, not withdrawn), adds the key with the invite's role,
    /// and hands over the membership history.
    async fn admit(&self, conn: &Connection, observed: Option<SocketAddr>) -> Result<Report, String> {
        let (name, home) = self.gate()?;
        let (name, home) = (name.as_str(), home.as_path());
        let joiner = to_hex(conn.remote_id().as_bytes());
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(|e| e.to_string())?;
        let Msg::Join { invite, listen, relay } = recv(&mut rx).await? else { return Err("protocol: expected join".into()) };
        let verdict = {
            let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
            invite::redeem(home, &self.me, name, &invite, &joiner, borg::learn::now_ms())
        };
        let m = match verdict {
            Ok(m) => m,
            Err(reason) => {
                send(&mut tx, &Msg::Refused { reason: reason.clone() }).await?;
                let _ = tx.finish();
                // Let the joiner read why before the connection goes (it closes once it has).
                let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                return Err(format!("join by {joiner}: {reason}"));
            }
        };
        steer::enforce(home, &m, borg::learn::now_ms());
        heard(home, &joiner, listen, relay, observed);
        send(&mut tx, &Msg::Welcome { docs: Membership::history(home) }).await?;
        let _ = tx.finish();
        conn.closed().await; // the joiner closes once it has read everything
        Ok(Report { joined: 1, ..Report::default() })
    }

    /// The next peer dialling in, `None` once the endpoint is closed.
    pub async fn incoming(&self) -> Option<Incoming> {
        self.endpoint.accept().await
    }

    /// Serves the next peer that dials in; returns what it moved, or why it was turned away.
    pub async fn accept(&self) -> Result<Report, String> {
        self.handle(self.incoming().await.ok_or("endpoint closed")?).await
    }

    /// Serves one peer that dialled in.
    pub async fn handle(&self, incoming: Incoming) -> Result<Report, String> {
        // The IP the dialler's packets came from: where it can be reached, whatever it bound to.
        let observed = match incoming.remote_addr() {
            IncomingAddr::Ip(a) => Some(a),
            _ => None,
        };
        let started = borg::learn::now_ms();
        let conn = incoming.await.map_err(|e| format!("handshake: {e}"))?;
        if conn.alpn() != ALPN && conn.alpn() != JOIN_ALPN {
            let (name, home) = self.gate()?;
            let remote = authorise(&conn, &members(&home)?, &name)?;
            return Err(self.mismatch(&conn, &remote));
        }
        let session = async {
            let (name, home) = self.gate()?;
            let (name, home) = (name.as_str(), home.as_path());
            let membership = members(home)?;
            let remote = authorise(&conn, &membership, name)?;
            let (mut tx, mut rx) = conn.accept_bi().await.map_err(|e| e.to_string())?;
            let Msg::Hello { collective, listen, relay } = recv(&mut rx).await? else { return Err("protocol: expected hello".into()) };
            if collective != name {
                let reason = format!("this node serves collective {name:?}, not {collective:?}");
                send(&mut tx, &Msg::Refused { reason: reason.clone() }).await?;
                return Err(reason);
            }
            heard(home, &remote, listen, relay, observed);
            send(&mut tx, &Msg::Hello { collective: name.into(), listen: self.listen(), relay: self.announced() }).await?;
            let report = self.exchange(false, &mut tx, &mut rx, membership, &remote, &[]).await?;
            let _ = tx.finish();
            conn.closed().await; // the dialler closes once it has read everything
            self.synced(&remote, started);
            Ok(report)
        };
        let result = if conn.alpn() == JOIN_ALPN { self.admit(&conn, observed).await } else { session.await };
        if let Err(e) = &result {
            conn.close(1u32.into(), e.as_bytes());
        }
        vectors::reindexed(&self.home, result).await
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }

    /// The session after both sides know who they talk to (module doc, steps 2–4).
    async fn exchange(
        &self,
        dialler: bool,
        tx: &mut SendStream,
        rx: &mut RecvStream,
        mine: Membership,
        remote: &str,
        ask: &[String],
    ) -> Result<Report, String> {
        let home = self.home.as_path();
        let mut report = Report::default();
        let Msg::Membership { docs } = swap(dialler, tx, rx, Msg::Membership { docs: Membership::history(home) }).await? else {
            return Err("protocol: expected membership".into());
        };
        let mine = {
            let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let (held, _) = Membership::adopt(home, &docs).unwrap_or((mine, false));
            report.quarantined += origin::apply_if_new(home, &held)?;
            steer::enforce(home, &held, borg::learn::now_ms()); // a signer who lost the right to steer stops steering here
            held
        };
        if mine.role_of(remote).is_none() {
            return Err(format!("{remote} is no longer a member of {}", mine.collective));
        }
        self.stamp()?;
        let origins = origin::load(home);

        // Lessons: reconcile with everyone.
        for file in SYNCED {
            self.reconcile(dialler, tx, rx, &home.join(file), &origins, &mine, &mut report).await?;
        }

        // A 0.3.0 peer's hive goals (3b): followed, nothing kept.
        let Msg::Goals { ids } = swap(dialler, tx, rx, Msg::Goals { ids: Vec::new() }).await? else {
            return Err("protocol: expected goals".into());
        };
        for goal in legacy_goals(ids) {
            for log in 0..LEGACY_GOAL_LOGS {
                self.skip(dialler, tx, rx, (remote.to_string(), goal.clone(), log)).await?;
            }
        }

        // Reasoning: each author hands its excerpts to their replicas.
        let path = home.join(REASONING);
        let held = lines::read(&path);
        let set = origin::admitted(held.clone(), &origins, &mine);
        self.gate()?;
        let me = self.me.public();
        let offer: BTreeSet<Hash> =
            set.keys().filter(|h| origins.get(*h).is_some_and(|o| o.key == me) && replicas(h, &mine, &me).iter().any(|k| k == remote)).copied().collect();
        let Msg::Offer { hashes } = swap(dialler, tx, rx, Msg::Offer { hashes: offer.iter().map(|h| to_hex(h)).collect() }).await? else {
            return Err("protocol: expected offer".into());
        };
        let offered: BTreeSet<Hash> = hashes.iter().map(|h| from_hex::<32>(h)).collect::<Result<_, _>>()?;
        let want: BTreeSet<Hash> = offered.into_iter().filter(|h| !held.contains_key(h)).collect();
        let Msg::Want { hashes } = swap(dialler, tx, rx, Msg::Want { hashes: want.iter().map(|h| to_hex(h)).collect() }).await? else {
            return Err("protocol: expected want".into());
        };
        let wanted: Vec<Hash> = hashes.iter().map(|h| from_hex::<32>(h)).collect::<Result<Vec<_>, _>>()?.into_iter().filter(|h| offer.contains(h)).collect();
        let give = signed(&set, &origins, wanted.iter());
        report.replicated += give.len();
        self.gate()?; // again just before lines leave
        let Msg::Lines { lines: got } = swap(dialler, tx, rx, Msg::Lines { lines: give }).await? else {
            return Err("protocol: expected lines".into());
        };
        self.store_lines(&path, got, &want, &mine, Some(remote), &mut report)?;

        // Excerpts by commit: any holder answers, whoever wrote them.
        let ask: Vec<String> = ask.iter().take(MAX_ASK).cloned().collect();
        let Msg::Ask { commits: theirs } = swap(dialler, tx, rx, Msg::Ask { commits: ask.clone() }).await? else {
            return Err("protocol: expected ask".into());
        };
        let held = lines::read(&path);
        let set = origin::admitted(held.clone(), &origin::load(home), &mine);
        let answer: Vec<&Hash> = set.iter().filter(|(_, l)| about(&theirs, l)).map(|(h, _)| h).take(MAX_ASK).collect();
        let give = signed(&set, &origin::load(home), answer.into_iter());
        report.answered += give.len();
        self.gate()?; // again just before lines leave
        let Msg::Lines { lines: got } = swap(dialler, tx, rx, Msg::Lines { lines: give }).await? else {
            return Err("protocol: expected lines".into());
        };
        let want: BTreeSet<Hash> = got.iter().filter(|s| about(&ask, &s.line)).map(|s| lines::hash(&s.line)).filter(|h| !held.contains_key(h)).collect();
        self.store_lines(&path, got, &want, &mine, None, &mut report)?;

        // Live: presence and steering, ephemeral (live.rs, steer.rs).
        let now = borg::learn::now_ms();
        let live = Msg::Live { presence: live::outgoing(home, &self.me, now), directives: steer::outgoing(home, now) };
        self.gate()?; // again just before anything leaves
        let Msg::Live { presence, directives } = swap(dialler, tx, rx, live).await? else {
            return Err("protocol: expected live".into());
        };
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        report.presence += live::receive(home, &mine, &me, presence, now)?;
        let (new, dropped) = steer::receive(home, &mine, &me, &directives, now);
        (report.steered, report.dropped) = (report.steered + new, report.dropped + dropped);
        borg::veto::apply_inbox(home); // a veto for a drone of this node: its credit and episode, once
        Ok(report)
    }

    /// One log reconciled with the peer (step 3): the admitted set, recon rounds, then the lines each
    /// side lacks, signed, stored when their origin verifies and is admitted.
    #[allow(clippy::too_many_arguments)]
    async fn reconcile(
        &self,
        dialler: bool,
        tx: &mut SendStream,
        rx: &mut RecvStream,
        path: &Path,
        origins: &HashMap<Hash, Origin>,
        mine: &Membership,
        report: &mut Report,
    ) -> Result<(), String> {
        let set = origin::admitted(lines::read(path), origins, mine);
        self.gate()?; // read first, then gate: a line written after a switch to local is never sent
        let mut r = Recon::new(&set);
        loop {
            let m = r.message();
            let Msg::Round { round } = swap(dialler, tx, rx, Msg::Round { round: m.clone() }).await? else {
                return Err("protocol: expected a reconciliation round".into());
            };
            if r.absorb(&m, round)? {
                break;
            }
        }
        (report.rounds, report.fps, report.hashes) = (report.rounds + r.cost.rounds, report.fps + r.cost.fps, report.hashes + r.cost.hashes);
        let give = within_budget(signed(&set, origins, r.give.iter()));
        report.sent += give.len();
        self.gate()?; // again just before lines leave
        let Msg::Lines { lines: got } = swap(dialler, tx, rx, Msg::Lines { lines: give }).await? else {
            return Err("protocol: expected lines".into());
        };
        self.store_lines(path, got, &r.want, mine, None, report)
    }

    /// One log of a 0.3.0 peer's hive goal (3b), followed in step with it and kept nowhere. The listener
    /// answers the peer's first round with that round itself, which closes it with nothing to give
    /// either way. The dialler speaks first, so it reconciles the hashes this peer gave before (`legacy`)
    /// and remembers the ones it gives now: the peer sends each line once per process, not per session.
    async fn skip(&self, dialler: bool, tx: &mut SendStream, rx: &mut RecvStream, log: (String, String, usize)) -> Result<(), String> {
        let mut want = BTreeSet::new();
        if dialler {
            let given = self.legacy.lock().unwrap_or_else(|e| e.into_inner()).get(&log).cloned().unwrap_or_default();
            let given: BTreeMap<Hash, ()> = given.into_iter().map(|h| (h, ())).collect();
            let mut r = Recon::new(&given);
            loop {
                let m = r.message();
                let Msg::Round { round } = swap(true, tx, rx, Msg::Round { round: m.clone() }).await? else {
                    return Err("protocol: expected a reconciliation round".into());
                };
                if r.absorb(&m, round)? {
                    break;
                }
            }
            want = r.want;
        } else {
            let Msg::Round { round } = recv(rx).await? else {
                return Err("protocol: expected a reconciliation round".into());
            };
            send(tx, &Msg::Round { round }).await?;
        }
        let Msg::Lines { lines: got } = swap(dialler, tx, rx, Msg::Lines { lines: Vec::new() }).await? else {
            return Err("protocol: expected lines".into());
        };
        let got: Vec<Hash> = got.iter().map(|s| lines::hash(&s.line)).filter(|h| want.contains(h)).collect();
        if !got.is_empty() {
            self.legacy.lock().unwrap_or_else(|e| e.into_inner()).entry(log).or_default().extend(got);
        }
        Ok(())
    }

    /// Stores the lines that were asked for, whose origin verifies and is admitted (and, for
    /// replicas, is the peer itself): origin records first, so a line is never re-stamped as ours.
    fn store_lines(
        &self,
        path: &Path,
        got: Vec<Signed>,
        want: &BTreeSet<Hash>,
        m: &Membership,
        author: Option<&str>,
        report: &mut Report,
    ) -> Result<(), String> {
        let mut keep: BTreeMap<Hash, Signed> = BTreeMap::new();
        for s in got {
            let h = lines::hash(&s.line);
            let ok = want.contains(&h) && s.origin.verify(&s.line).is_ok() && m.admits(&s.origin.key, s.origin.ms) && author.is_none_or(|a| a == s.origin.key);
            if ok {
                keep.insert(h, s);
            } else {
                report.rejected += 1;
            }
        }
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let known = origin::load(&self.home);
        origin::append(&self.home, &keep.iter().filter(|(h, _)| !known.contains_key(*h)).map(|(_, s)| s.origin.clone()).collect::<Vec<_>>())?;
        report.received += lines::merge(path, &keep.into_values().map(|s| s.line).collect::<Vec<_>>())?;
        Ok(())
    }
}

/// The goals a 0.3.0 peer reconciles after naming `ids` while this node names none, in its order: the
/// distinct valid goal ids (its `goal::valid_id`: `[a-z0-9-]{1,40}`), sorted.
fn legacy_goals(ids: Vec<String>) -> BTreeSet<String> {
    let valid = |id: &String| (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    ids.into_iter().filter(valid).collect()
}

/// Where a dialler that announced `listen` can be reached: the IP its packets came from (what
/// QUIC observed) with the port it listens on. Nothing for an address nobody could dial.
fn reachable(listen: &str, observed: Option<SocketAddr>) -> Option<SocketAddr> {
    let listen: SocketAddr = listen.parse().ok()?;
    let ip = match observed {
        Some(o) => o.ip().to_canonical(),
        None => listen.ip(),
    };
    let at = SocketAddr::new(ip, listen.port());
    (!at.ip().is_unspecified() && at.port() != 0).then_some(at)
}

/// Remembers a member that dialled in where it can be dialled back: the IP its packets came from
/// with the port it listens on, and the home relay it announced. Known by key at least.
fn heard(home: &Path, key: &str, listen: Option<String>, relay: Option<String>, observed: Option<SocketAddr>) {
    let direct = listen.and_then(|l| reachable(&l, observed)).map(|a| a.to_string());
    let hints: Vec<String> = direct.into_iter().chain(relay.filter(|r| addr::hints(r).is_ok())).collect();
    let _ = peers::remember(home, key, &hints.join(","));
}

/// Whether a line of reasoning.jsonl is about one of `commits`: an excerpt by its `commit`, a
/// decision → fix link record (borg notes/links.rs) by either side, so fetching the fix brings it too.
fn about(commits: &[String], line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return false };
    ["commit", "fix"].iter().filter_map(|k| v.get(k)?.as_str()).any(|c| asked(commits, c))
}

/// Whether `commit` is one of `asked` (full ids, or abbreviations of at least 7 characters).
fn asked(asked: &[String], commit: &str) -> bool {
    asked.iter().any(|a| a == commit || (a.len() >= 7 && commit.starts_with(a.as_str())))
}

/// The first of `lines` whose sizes add up to at most `MAX_GIVE` (at least one).
fn within_budget(lines: Vec<Signed>) -> Vec<Signed> {
    let mut used = 0;
    lines
        .into_iter()
        .enumerate()
        .take_while(|(i, s)| {
            used += s.line.len();
            *i == 0 || used <= MAX_GIVE
        })
        .map(|(_, s)| s)
        .collect()
}

fn signed<'a>(set: &BTreeMap<Hash, String>, origins: &HashMap<Hash, Origin>, hashes: impl Iterator<Item = &'a Hash>) -> Vec<Signed> {
    hashes.filter_map(|h| Some(Signed { line: set.get(h)?.clone(), origin: origins.get(h)?.clone() })).collect()
}

/// The members (other than `author`) that hold a replica of excerpt `h`: the `REPLICAS` highest
/// `blake3(h, key)` (rendezvous hashing: every node computes the same choice, and a membership
/// change moves only the excerpts of the members it touches).
pub fn replicas(h: &Hash, m: &Membership, author: &str) -> Vec<String> {
    let mut keys: Vec<(Hash, &str)> = m
        .members
        .iter()
        .filter(|x| x.key != author)
        .map(|x| {
            let mut s = blake3::Hasher::new();
            s.update(h);
            s.update(x.key.as_bytes());
            (*s.finalize().as_bytes(), x.key.as_str())
        })
        .collect();
    keys.sort_by(|a, b| b.cmp(a));
    keys.into_iter().take(REPLICAS).map(|(_, k)| k.to_string()).collect()
}

fn members(home: &Path) -> Result<Membership, String> {
    Membership::load(home)?
        .ok_or_else(|| format!("no membership in {}: `borg mesh init` (admin) or `borg mesh join <file>`", crate::membership::path(home).display()))
}

/// The key iroh authenticated, if the membership lists it.
fn authorise(conn: &Connection, membership: &Membership, name: &str) -> Result<String, String> {
    let remote = to_hex(conn.remote_id().as_bytes());
    match membership.role_of(&remote) {
        Some(_) => Ok(remote),
        None => Err(format!("rejected {remote}: not a member of {name}")),
    }
}

/// The dialler sends then reads; the listener reads (and could refuse) then sends.
async fn swap(dialler: bool, tx: &mut SendStream, rx: &mut RecvStream, m: Msg) -> Result<Msg, String> {
    if dialler {
        send(tx, &m).await?;
        recv(rx).await
    } else {
        let theirs = recv(rx).await?;
        send(tx, &m).await?;
        Ok(theirs)
    }
}

async fn send(tx: &mut SendStream, m: &Msg) -> Result<(), String> {
    let body = serde_json::to_vec(m).map_err(|e| e.to_string())?;
    tx.write_all(&(body.len() as u32).to_be_bytes()).await.map_err(|e| format!("send: {e}"))?;
    tx.write_all(&body).await.map_err(|e| format!("send: {e}"))
}

async fn recv(rx: &mut RecvStream) -> Result<Msg, String> {
    let mut len = [0u8; 4];
    rx.read_exact(&mut len).await.map_err(|e| format!("receive: {e}"))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(format!("frame of {len} bytes exceeds {MAX_FRAME}"));
    }
    let mut body = vec![0u8; len];
    rx.read_exact(&mut body).await.map_err(|e| format!("receive: {e}"))?;
    match serde_json::from_slice(&body).map_err(|e| format!("protocol: {e}"))? {
        Msg::Refused { reason } => Err(format!("refused by peer: {reason}")),
        m => Ok(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An excerpt is asked for by its commit; a link record by its decision or by its fix.
    #[test]
    fn lines_about_the_asked_commits() {
        let ask = vec!["d0d0d0d".to_string(), "f1f1f1f1f1".to_string()];
        let excerpt = json!({"kind": "reasoning", "commit": "d0d0d0d0d0"}).to_string();
        let link = |c: &str, f: &str| json!({"kind": "regress", "commit": c, "fix": f}).to_string();
        assert!(about(&ask, &excerpt) && about(&ask, &link("aaaaaaaaaa", "f1f1f1f1f1")) && about(&ask, &link("d0d0d0d0d0", "bbbbbbbbbb")));
        assert!(!about(&ask, &link("aaaaaaaaaa", "bbbbbbbbbb")) && !about(&ask, "not json"));
    }

    /// A frame of lines stays within `MAX_GIVE`: the rest wait for the next session; one line always goes.
    #[test]
    fn a_frame_gives_lines_up_to_its_budget() {
        let me = Identity::load_or_create(&std::env::temp_dir().join(format!("borg-mesh-budget-{}", std::process::id()))).unwrap();
        let line = |n: usize| {
            let l = "x".repeat(n);
            Signed { origin: Origin::new(&me, &lines::hash(&l), 1), line: l }
        };
        let give = within_budget(vec![line(MAX_GIVE / 2), line(MAX_GIVE / 2), line(1)]);
        assert_eq!(give.len(), 2);
        assert_eq!(within_budget(vec![line(MAX_GIVE + 1), line(1)]).len(), 1);
        assert!(within_budget(Vec::new()).is_empty());
    }

    /// One node's machine: a root store (`BORG_HOME`), a config (`BORG_CONFIG`) and a repo that
    /// routes to collective "work" with `share`.
    fn node(tag: &str, share: &str) -> Place {
        let base = std::env::temp_dir().join(format!("borg-mesh-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(home_of(&base)).unwrap();
        let p = Place { config: base.join("collectives.json"), base: base.clone(), dir: base.join("repo").display().to_string(), collective: None };
        configure(&p, share);
        p
    }

    fn configure(p: &Place, share: &str) {
        let cfg = json!({"default": "off", "collectives": [{"name": "work", "paths": [p.dir], "share": share, "relay": "disabled"}]});
        std::fs::write(&p.config, cfg.to_string()).unwrap();
    }

    fn home_of(base: &Path) -> PathBuf {
        base.join("collectives").join("work")
    }

    fn home(p: &Place) -> PathBuf {
        home_of(&p.base)
    }

    fn lesson(id: &str, version: u32, q: &str) -> String {
        json!({"id": id, "origin_ms": 1, "question": q, "version": version}).to_string()
    }

    fn write(home: &Path, file: &str, lines: &[String]) {
        std::fs::write(home.join(file), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    }

    const LOCAL: &str = "127.0.0.1:0";

    /// Alice founds the collective and invites Bob, who installs the document she hands him.
    fn team(tag: &str) -> (Place, Place) {
        let (a, b) = (node(&format!("{tag}-alice"), "team"), node(&format!("{tag}-bob"), "team"));
        let alice = Identity::load_or_create(&home(&a)).unwrap();
        let bob = Identity::load_or_create(&home(&b)).unwrap();
        let mut m = Membership::found("work", &alice);
        m.invite(&alice, &bob.public(), "member").unwrap();
        m.save(&home(&a)).unwrap();
        m.save(&home(&b)).unwrap();
        (a, b)
    }

    fn rollup(node: &str, version: u64, turns: u64) -> String {
        json!({"id": format!("stat:{node}:dev@x:2026-10-12"), "version": version, "day": "2026-10-12", "node": node,
            "engineer": "dev@x", "counts": {"turn_end": turns}, "usd": 0.0, "dur_ms": 0})
        .to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn usage_rollups_sync_and_the_latest_version_wins() {
        let (pa, pb) = team("stats");
        let (a, b) = (home(&pa), home(&pb));
        write(&a, "stats.jsonl", &[rollup("na", 1, 3), rollup("na", 2, 5)]);
        write(&b, "stats.jsonl", &[rollup("nb", 1, 7)]);
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let (served, synced) = (served.unwrap(), synced.unwrap());
        assert_eq!((synced.received, served.received), (2, 1), "both of alice's lines reach bob, bob's one reaches alice");
        for h in [&a, &b] {
            let r = borg::stats::rollups(h);
            assert_eq!(r.len(), 2, "one bucket per node");
            assert_eq!(r["stat:na:dev@x:2026-10-12"].counts["turn_end"], 5, "the newest version of alice's bucket");
            assert_eq!(r["stat:nb:dev@x:2026-10-12"].counts["turn_end"], 7);
        }
    }

    /// Each node promoted an edit to the same playbook; after a round both fold the same steps, in the
    /// same order, under the same version.
    #[tokio::test(flavor = "multi_thread")]
    async fn playbook_edits_sync_and_fold_the_same_on_both_nodes() {
        let (pa, pb) = team("playbooks");
        let (a, b) = (home(&pa), home(&pb));
        let edit = |step: &str, text: &str, ms: i64, ok: &str| {
            let mut e = borg::playbook::Edit {
                pb: borg::playbook::playbook_id("", "fix the parser"),
                step: step.into(),
                text: text.into(),
                shape: "fix the parser".into(),
                ms,
                ok_by: ok.into(),
                ..Default::default()
            };
            e.edit = borg::playbook::edit_id(&e);
            serde_json::to_string(&e).unwrap()
        };
        write(&a, borg::playbook::FILE, &[edit("s1", "run the parser tests", 1, "q1")]);
        write(&b, borg::playbook::FILE, &[edit("s2", "add a failing test first", 2, "q2")]);
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        let (fa, fb) = (borg::playbook::read(&a), borg::playbook::read(&b));
        assert_eq!(fa, fb);
        assert_eq!(fa[0].steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
        assert_eq!(fa[0].key(), fb[0].key());
        alice.close().await;
        bob.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ratified_constraint_reaches_the_team_signed() {
        let (pa, pb) = team("constraints");
        let (a, b) = (home(&pa), home(&pb));
        write(&a, "lessons.jsonl", &[lesson("L1", 1, "lock order?")]);
        let c = json!({"id": "cabc", "text": "locks are taken in id order", "lesson": "L1", "by": "lead@x", "ms": 5, "source": "veto"}).to_string();
        write(&a, borg::veto::FILE, std::slice::from_ref(&c));
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        assert!(synced.unwrap().received >= 2, "the lesson and the constraint");
        assert_eq!(lines::read(&b.join(borg::veto::FILE)).into_values().collect::<Vec<_>>(), std::slice::from_ref(&c));
        assert!(origin::load(&b).contains_key(&lines::hash(&c)), "it arrived with its signed origin");
        let standing: Vec<String> = borg::conscience::load(&b.join("lessons.jsonl"), None).into_iter().filter(|c| c.standing()).map(|c| c.question).collect();
        assert_eq!(standing, ["locks are taken in id order"], "bob's sessions get it as a standing lesson");
        alice.close().await;
        bob.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn team_outcomes_sync_and_count_once() {
        let (pa, pb) = team("outcomes");
        let (a, b) = (home(&pa), home(&pb));
        write(&a, "lessons.jsonl", &[lesson("L1", 1, "lock order?")]);
        let o = |kind: &str, ms| json!({"kind": kind, "session": "s1", "lessons": ["L1"], "ms": ms, "repo": "app", "groups": [["L1", "L2"]]}).to_string();
        write(&a, "outcomes.jsonl", &[o("asked", 1), o("asked_then_changed", 2)]);
        assert_eq!(borg::outcome::publish(&a), 2);
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        let team = borg::outcome::TEAM_FILE;
        assert_eq!(lines::read(&b.join(team)), lines::read(&a.join(team)), "bob holds alice's outcomes");
        let text = std::fs::read_to_string(b.join(team)).unwrap();
        assert!(!text.contains("\"s1\"") && !text.contains("app") && !text.contains("groups"), "redacted: {text}");
        assert!(!b.join("outcomes.jsonl").exists(), "the raw log stays with alice");
        let l1 = [borg::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let changed = |h: &Path| borg::memory::derive(&borg::outcome::read_team(&h.join("lessons.jsonl")), &l1, None)["L1"].changed;
        assert_eq!(changed(&b), 1, "alice's session counts on bob");
        assert_eq!(changed(&a), 1, "alice's own published copy does not count twice");
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let quiet = Report { rounds: SYNCED.len(), fps: SYNCED.len(), ..Report::default() };
        assert_eq!((synced.unwrap(), served.unwrap()), (quiet, quiet), "resync is quiet");
        alice.close().await;
        bob.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_members_end_with_the_union_and_resync_moves_only_digests() {
        let (pa, pb) = team("union");
        let (a, b) = (home(&pa), home(&pb));
        write(&a, "lessons.jsonl", &[lesson("L1", 1, "lock order?"), lesson("L2", 1, "retry budget?")]);
        write(&b, "lessons.jsonl", &[lesson("L1", 1, "lock order?"), lesson("L1", 2, "lock order, also in tests?"), lesson("L3", 1, "idempotent?")]);

        // Since bob joined, alice invited carol: bob learns of her through the sync (trust first).
        let carol = Identity::load_or_create(&home(&node("union-carol", "team"))).unwrap().public();
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.invite(&Identity::load_or_create(&a).unwrap(), &carol, "member").unwrap();
        m.save(&a).unwrap();

        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let (served, synced) = (served.unwrap(), synced.unwrap());
        assert_eq!((synced.received, synced.sent), (1, 2), "bob gets L2, gives L1v2 and L3");
        assert_eq!((served.received, served.sent), (2, 1));
        assert_eq!(Membership::load(&b).unwrap().unwrap().role_of(&carol), Some("member"), "bob adopted the newer membership");
        for file in SYNCED {
            assert_eq!(lines::read(&a.join(file)), lines::read(&b.join(file)), "{file}: both hold the union");
        }
        let asked = |h: &Path| {
            let mut v: Vec<(String, String)> = borg::conscience::load(&h.join("lessons.jsonl"), None).into_iter().map(|c| (c.id, c.question)).collect();
            v.sort();
            v
        };
        assert_eq!(asked(&a), asked(&b));
        assert!(asked(&a).contains(&("L1".into(), "lock order, also in tests?".into())), "the newer version wins on alice too");

        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        // One round and one fingerprint per synced log (lessons, stats, outcomes), nothing else.
        let quiet = Report { rounds: SYNCED.len(), fps: SYNCED.len(), ..Report::default() };
        assert_eq!(synced.unwrap(), quiet, "re-sync: one fingerprint per log, nothing new");
        assert_eq!(served.unwrap(), quiet);
        alice.close().await;
        bob.close().await;
    }

    /// Each episode is learned once, by the node that captured it: before alice's lesson arrives
    /// bob has nothing to learn, afterwards he holds her lesson and still asks no model.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_episode_is_learned_only_where_it_was_captured() {
        let (pa, pb) = team("episodes");
        let (a, b) = (home(&pa), home(&pb));
        let e1 = json!({"id": "e1", "ms": 1, "repo": "r", "session": "s", "task": "t", "agent": "a",
            "rejected_tool": null, "correction": "check TOCTOU on the flag", "engineer": "ann@x"});
        write(&a, "episodes.jsonl", &[e1.to_string()]);
        let calls = std::cell::Cell::new(0);
        let ask = |_: &str| {
            calls.set(calls.get() + 1);
            Some(json!({"is_lesson": true, "question": "Is the check-then-act atomic?", "situations": [], "consequence": "race", "same_as": ""}))
        };

        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        assert!(!b.join("episodes.jsonl").exists(), "alice's raw episode stays with alice");
        assert_eq!(borg::learn::learn_in(&b, None, &ask, false, 2), (0, 0, 0));
        assert_eq!(calls.get(), 0, "bob asks no model for alice's episode");

        assert_eq!(borg::learn::learn_in(&a, None, &ask, false, 2), (1, 0, 0));
        assert_eq!(calls.get(), 1, "alice learns e1 once");
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        assert_eq!(synced.unwrap().received, 1, "bob receives the lesson");
        let got = borg::learn::lessons(&b.join("lessons.jsonl"));
        assert_eq!(
            (got.len(), got[0].id.as_str(), got[0].episodes.clone(), got[0].engineers.clone()),
            (1, "Le1", vec!["e1".to_string()], vec!["ann@x".to_string()])
        );
        assert_eq!(borg::learn::learn_in(&b, None, &ask, false, 3), (0, 0, 0));
        assert_eq!(calls.get(), 1, "and still learns nothing again");
        alice.close().await;
        bob.close().await;
    }

    /// A member on an older protocol (a node that only speaks `borg/mesh/5`) is told apart from a broken
    /// network both ways: the session ends at once, the error and the record (for `borg doctor`) name
    /// the peer and who has to upgrade, and the first session that completes drops the record.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_on_an_older_protocol_is_named_both_ways() {
        const OLD: &[u8] = b"borg/mesh/5";
        let (pa, pb) = team("protocol");
        let (a, b) = (home(&pa), home(&pb));
        let bob_key = Identity::load_or_create(&b).unwrap().public();
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let old_bob = Node::open(&pb, Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![OLD.to_vec()])).await.unwrap();

        // Alice dials the old node.
        let serve_old = async {
            let conn = old_bob.incoming().await?.await.ok()?;
            Some((conn.alpn().to_vec(), conn.closed().await.to_string()))
        };
        let old_ticket = old_bob.ticket();
        let (seen, synced) = tokio::join!(serve_old, alice.sync(&old_ticket));
        let err = synced.unwrap_err();
        let ours = String::from_utf8_lossy(ALPN);
        let line = format!("peer {} speaks borg/mesh/5, this node {ours}: nothing syncs with it; upgrade it", &bob_key[..12]);
        assert_eq!(err, line);
        let (alpn, closed) = seen.expect("the old node completed the handshake");
        assert_eq!(alpn, OLD);
        let alice_key = Identity::load_or_create(&a).unwrap().public();
        let told = format!("peer {} speaks {ours}, this node borg/mesh/5: nothing syncs with it; upgrade this node", &alice_key[..12]);
        assert!(closed.contains(&told), "the old node is told why, as it reads it: {closed}");
        assert_eq!(borg::presence::mismatches(&a)[&bob_key].theirs, "borg/mesh/5");

        // The old node dials alice.
        let _ = std::fs::remove_file(borg::presence::protocols_path(&a));
        let addr = Target::parse(&alice.ticket()).unwrap().endpoint_addr(&old_bob.relay).unwrap();
        let dial_old = async {
            let conn = old_bob.endpoint.connect(addr, OLD).await.map_err(|e| e.to_string())?;
            Ok::<String, String>(conn.closed().await.to_string())
        };
        let (served, dialled) = tokio::join!(alice.accept(), dial_old);
        assert_eq!(served.unwrap_err(), line);
        assert!(dialled.unwrap().contains(&told));
        assert_eq!(borg::presence::mismatches(&a)[&bob_key].ours, ours);
        old_bob.close().await;

        // Bob upgraded: the first session that completes drops the record.
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        assert!(borg::presence::mismatches(&a).is_empty());
        alice.close().await;
        bob.close().await;
    }

    #[test]
    fn older_protocols_are_every_version_below_this_one() {
        let n = version(ALPN).expect("borg/mesh/<n>");
        let o = older();
        let below = format!("borg/mesh/{}", n - 1);
        assert_eq!((o.len(), o[0].as_slice(), o[o.len() - 1].as_slice()), (n as usize - 1, below.as_bytes(), b"borg/mesh/1".as_slice()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlisted_key_is_rejected_both_ways() {
        let (pa, pb) = team("unlisted");
        let a = home(&pa);
        write(&a, "lessons.jsonl", &[lesson("L1", 1, "secret sauce?")]);
        // Eve forges her own "work" membership that lists alice, but alice's does not list eve.
        let pe = node("unlisted-eve", "team");
        let e = home(&pe);
        let eve = Identity::load_or_create(&e).unwrap();
        let mut forged = Membership::found("work", &eve);
        forged.invite(&eve, &Identity::load_or_create(&a).unwrap().public(), "member").unwrap();
        forged.save(&e).unwrap();

        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let eve_node = Node::bind(&pe, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), eve_node.sync(&ticket));
        assert!(served.unwrap_err().contains("not a member"), "alice turns eve away");
        assert!(synced.is_err());
        assert!(!e.join("lessons.jsonl").exists(), "eve received nothing");

        // And a member never dials a key its membership does not list.
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let err = bob.sync(&eve_node.ticket()).await.unwrap_err();
        assert!(err.contains("not a member") && err.contains("refusing to connect"), "{err}");
        for n in [alice, eve_node, bob] {
            n.close().await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_collective_or_off_repo_never_syncs() {
        let (pa, _) = team("local");
        write(&home(&pa), "lessons.jsonl", &[lesson("L1", 1, "private thought?")]);
        configure(&pa, "local");
        assert!(!collective::may_sync(&pa.route()));
        let err = Node::bind(&pa, LOCAL.parse().unwrap()).await.err().expect("local must be refused");
        assert!(err.contains("is local") && err.contains("refusing to sync"), "{err}");
        let off = Place { dir: "/somewhere/else".into(), ..pa.clone() };
        let err = Node::bind(&off, LOCAL.parse().unwrap()).await.err().expect("off must be refused");
        assert!(err.contains("off"), "{err}");
        std::fs::write(&pa.config, "{ broken").unwrap();
        assert_eq!(pa.route(), Route::Off, "a broken config routes off");
        assert!(Node::bind(&pa, LOCAL.parse().unwrap()).await.is_err());
        let team = Route::Collective { name: "work".into(), home: home(&pa), share: collective::Share::Team };
        assert!(gate(&team).is_ok() && gate(&Route::Off).is_err());
    }

    /// A running `serve` re-reads the config at each session: once the collective is set to
    /// `share: local`, the next peer is refused before a single frame of it is sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn serve_stops_syncing_when_the_collective_turns_local() {
        let (pa, pb) = team("turns-local");
        write(&home(&pa), "lessons.jsonl", &[lesson("L1", 1, "now private?")]);
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        configure(&pa, "local");
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let err = served.unwrap_err();
        assert!(err.contains("is local") && err.contains("refusing to sync"), "{err}");
        assert!(synced.is_err());
        assert!(!home(&pb).join("lessons.jsonl").exists(), "bob received nothing");
        // A dialling node is gated the same way.
        configure(&pb, "local");
        assert!(bob.sync(&ticket).await.unwrap_err().contains("is local"));
        alice.close().await;
        bob.close().await;
    }

    /// Second guard: a team route whose store a local collective also maps to never syncs, even if
    /// such a config got past `Config::check`.
    #[test]
    fn a_store_shared_with_a_local_collective_is_refused() {
        let p = node("shared-store", "team");
        let cfg: Config = serde_json::from_value(json!({"collectives": [
            {"name": "work", "paths": [p.dir], "share": "team"},
            {"name": "work", "paths": ["/elsewhere"], "share": "local"}]}))
        .unwrap();
        let route = collective::route(Some(&cfg), &p.dir, &p.base);
        assert!(gate(&route).is_ok(), "the route alone looks shareable");
        assert_eq!(collective::local_owner_of(Some(&cfg), &home(&p), &p.base), Some("work"));
        // Written to disk, load rejects the config outright, which routes off.
        std::fs::write(
            &p.config,
            serde_json::to_string(&json!({"collectives": [
            {"name": "work", "paths": [p.dir], "share": "team"},
            {"name": "work", "paths": ["/elsewhere"], "share": "local"}]}))
            .unwrap(),
        )
        .unwrap();
        assert!(p.gate().is_err());
    }

    /// Alice founds the collective and invites everyone else; all install the same document.
    fn crew(tag: &str, names: &[&str]) -> Vec<Place> {
        let places: Vec<Place> = names.iter().map(|n| node(&format!("{tag}-{n}"), "team")).collect();
        let ids: Vec<Identity> = places.iter().map(|p| Identity::load_or_create(&home(p)).unwrap()).collect();
        let mut m = Membership::found("work", &ids[0]);
        for id in &ids[1..] {
            m.invite(&ids[0], &id.public(), "member").unwrap();
        }
        for p in &places {
            m.save(&home(p)).unwrap();
        }
        places
    }

    fn append_lines(home: &Path, file: &str, lines: &[String]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(home.join(file)).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn holds(home: &Path, file: &str, line: &str) -> bool {
        lines::read(&home.join(file)).contains_key(&lines::hash(line))
    }

    async fn eventually(what: &str, limit: std::time::Duration, f: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !f() {
            assert!(start.elapsed() < limit, "timed out waiting for: {what}");
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    fn addr_of(n: &Node) -> String {
        n.ticket().split_once('@').unwrap().1.to_string()
    }

    /// Two `serve` loops: a lesson either side writes reaches the other with no manual sync, and
    /// once alice's collective turns local, her next lesson stays home. Both stop cleanly.
    #[tokio::test(flavor = "multi_thread")]
    async fn serve_spreads_new_lessons_without_a_manual_sync() {
        use crate::daemon::{self, Event, Options};
        use std::sync::Arc;
        use std::time::Duration;
        let p = crew("daemon", &["alice", "bob"]);
        let (a, b) = (home(&p[0]), home(&p[1]));
        let alice = Arc::new(Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap());
        let bob = Arc::new(Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap());
        // Bob knows where alice is; alice learns bob's address when he first dials in.
        assert!(peers::remember(&b, &alice.public(), &addr_of(&alice)).unwrap());
        let opts = Options {
            interval: Duration::from_millis(300),
            poll: Duration::from_millis(50),
            backoff_max: Duration::from_millis(600),
            timeout: Duration::from_secs(5),
            grace: Duration::from_secs(1),
        };
        let synced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut stops = Vec::new();
        let mut loops = Vec::new();
        for n in [alice.clone(), bob.clone()] {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            stops.push(tx);
            let count = synced.clone();
            loops.push(tokio::spawn(daemon::serve(
                n,
                opts,
                async move {
                    let _ = rx.await;
                },
                move |e| {
                    if matches!(e, Event::Synced { result: Ok(_), .. }) {
                        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                },
            )));
        }
        let l1 = lesson("L1", 1, "who owns the lock?");
        append_lines(&a, "lessons.jsonl", std::slice::from_ref(&l1));
        eventually("alice's lesson at bob", Duration::from_secs(10), || holds(&b, "lessons.jsonl", &l1)).await;
        let l2 = lesson("L2", 1, "is the retry idempotent?");
        append_lines(&b, "lessons.jsonl", std::slice::from_ref(&l2));
        eventually("bob's lesson at alice", Duration::from_secs(10), || holds(&a, "lessons.jsonl", &l2)).await;
        assert_eq!(peers::load(&a).get(&bob.public()), Some(&addr_of(&bob)), "alice learned bob's address from his hello");
        // The receiver holds a lesson once its served session wrote it; the dialler's round reports
        // `Synced` only after it closes, a moment later.
        eventually("a successful sync round", Duration::from_secs(10), || synced.load(std::sync::atomic::Ordering::Relaxed) > 0).await;

        // Alice's collective turns local: her loops stop dialling, bob is turned away.
        configure(&p[0], "local");
        let l3 = lesson("L3", 1, "private now?");
        append_lines(&a, "lessons.jsonl", std::slice::from_ref(&l3));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!holds(&b, "lessons.jsonl", &l3), "a local collective's lesson never leaves");

        for s in stops {
            let _ = s.send(());
        }
        for l in loops {
            tokio::time::timeout(Duration::from_secs(5), l).await.expect("serve stops within its grace").unwrap();
        }
    }

    /// `serve` loops for `nodes`, each stopped by its sender; the failures they report, with who and when.
    struct Serving {
        stops: Vec<tokio::sync::oneshot::Sender<()>>,
        loops: Vec<tokio::task::JoinHandle<()>>,
        failed: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    fn serving(nodes: Vec<(&'static str, std::sync::Arc<Node>)>, opts: crate::daemon::Options) -> Serving {
        let s = Serving { stops: Vec::new(), loops: Vec::new(), failed: Default::default() };
        nodes.into_iter().fold(s, |mut s, (who, n)| {
            s.add(who, n, opts);
            s
        })
    }

    impl Serving {
        fn add(&mut self, who: &'static str, n: std::sync::Arc<Node>, opts: crate::daemon::Options) {
            use crate::daemon::{self, Event};
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            self.stops.push(tx);
            let failed = self.failed.clone();
            self.loops.push(tokio::spawn(daemon::serve(
                n,
                opts,
                async move {
                    let _ = rx.await;
                },
                move |e| {
                    if matches!(e, Event::Served(Err(_)) | Event::Synced { result: Err(_), .. } | Event::Gate(Err(_))) {
                        failed.lock().unwrap().push(format!("{who} at {}: {e:?}", borg::learn::now_ms()));
                    }
                },
            )));
        }

        async fn stop(self) {
            for s in self.stops {
                let _ = s.send(());
            }
            for l in self.loops {
                tokio::time::timeout(std::time::Duration::from_secs(10), l).await.expect("serve stops within its grace").unwrap();
            }
        }
    }

    /// Appends a stats line `i` to alice's store (`a`) and waits for it in bob's (`b`): how long it took, in
    /// ms. Not a lesson: a received lesson re-indexes bob's vectors before the session ends (vectors.rs
    /// `reindexed`), which times the embedding model, not the dials.
    async fn stat_trip(a: &Path, b: &Path, i: u64, failed: &std::sync::Mutex<Vec<String>>) -> f64 {
        let l = rollup(&format!("n{i}"), 1, i);
        let at = std::time::Instant::now();
        append_lines(a, "stats.jsonl", std::slice::from_ref(&l));
        while !holds(b, "stats.jsonl", &l) {
            assert!(at.elapsed() < std::time::Duration::from_secs(10), "timed out waiting for alice's line {i} at bob; failures: {:?}", failed.lock().unwrap());
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        at.elapsed().as_secs_f64() * 1000.0
    }

    /// Carol is a member alice dials, but her endpoint is gone: her dials wait out their `timeout`
    /// (and come back after the first retry), and alice's lines still reach bob a `poll` later.
    /// Carol's session record from before goes with her first failed dial.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_does_not_answer_holds_no_line_back() {
        use std::sync::Arc;
        use std::time::Duration;
        let p = crew("deaf", &["alice", "bob", "carol"]);
        let (a, b) = (home(&p[0]), home(&p[1]));
        let alice = Arc::new(Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap());
        let bob = Arc::new(Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap());
        let carol = Node::bind(&p[2], LOCAL.parse().unwrap()).await.unwrap();
        let (key, at) = (carol.public(), addr_of(&carol));
        carol.close().await;
        assert!(peers::remember(&a, &key, &at).unwrap());
        assert!(peers::remember(&b, &alice.public(), &addr_of(&alice)).unwrap());
        borg::presence::note_synced(&a, &key, borg::learn::now_ms()).unwrap();
        let opts =
            crate::daemon::Options { poll: Duration::from_millis(100), timeout: Duration::from_secs(3), grace: Duration::from_secs(1), ..Default::default() };
        let serve = serving(vec![("alice", alice.clone()), ("bob", bob.clone())], opts);
        eventually("bob's address at alice", Duration::from_secs(10), || peers::load(&a).contains_key(&bob.public())).await;
        // Over 6 s: carol's first dial, its retry a `poll` after it failed, and the one after.
        let mut took = Vec::new();
        for i in 1..=15u64 {
            took.push(stat_trip(&a, &b, i, &serve.failed).await);
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        let failed = serve.failed.lock().unwrap().clone();
        assert!(failed.iter().any(|f| f.contains(&key)), "carol's dials failed: {failed:?}");
        assert!(!borg::presence::synced(&a).contains_key(&key), "a failed dial drops carol's record");
        serve.stop().await;
        // A line carol held back would wait out her 3 s `timeout`; a lost handshake packet costs one
        // trip about a second now and then (the transport's retransmit), so the bar is the median.
        assert!(took.iter().all(|&t| t < 3000.0), "no line waits for carol's dial, took {took:?}");
        let mut sorted = took.clone();
        sorted.sort_by(f64::total_cmp);
        assert!(sorted[sorted.len() / 2] < 500.0, "lines reach bob a `poll` or two later, took {took:?}");
    }

    /// Alice's first dial to bob fails (his node runs no `serve` yet); the lesson it was to carry reaches
    /// him a `poll` later, with no new change and bob never dialling in: not after a 2-minute backoff.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_dial_is_retried_soon_with_what_it_missed() {
        use std::sync::Arc;
        use std::time::Duration;
        let p = crew("retry", &["alice", "bob"]);
        let (a, b) = (home(&p[0]), home(&p[1]));
        let alice = Arc::new(Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap());
        let bob = Arc::new(Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap());
        assert!(peers::remember(&a, &bob.public(), &addr_of(&bob)).unwrap());
        let l1 = lesson("L1", 1, "who owns the lock?");
        append_lines(&a, "lessons.jsonl", std::slice::from_ref(&l1));
        let opts = crate::daemon::Options { timeout: Duration::from_secs(1), grace: Duration::from_secs(1), ..Default::default() };
        let mut serve = serving(vec![("alice", alice.clone())], opts);
        eventually("alice's dial to bob failed", Duration::from_secs(10), || !serve.failed.lock().unwrap().is_empty()).await;
        serve.add("bob", bob.clone(), opts);
        eventually("alice's lesson at bob", Duration::from_secs(10), || holds(&b, "lessons.jsonl", &l1)).await;
        serve.stop().await;
    }

    /// A 0.3.0 peer's goal ids (3b) count once each and only when valid, so both sides follow the
    /// same number of reconciliations: "../evil" or "A B" never names a goal.
    #[test]
    fn legacy_goals_count_the_valid_distinct_ids() {
        let ids = ["g1", "../evil", "A B", "g0", "g1", "/abs", ""].map(String::from).to_vec();
        assert_eq!(legacy_goals(ids).into_iter().collect::<Vec<_>>(), ["g0", "g1"], "sorted, as the peer walks them");
        assert!(legacy_goals(vec!["x".repeat(41)]).is_empty());
    }

    /// A 0.3.0 node's hive goal: its seven synced logs, held in memory, each line signed by the node.
    struct OldGoal {
        logs: Vec<BTreeMap<Hash, String>>,
        origins: HashMap<Hash, Origin>,
    }

    impl OldGoal {
        fn new(me: &Identity) -> OldGoal {
            let mut g = OldGoal { logs: vec![BTreeMap::new(); LEGACY_GOAL_LOGS], origins: HashMap::new() };
            for log in 0..LEGACY_GOAL_LOGS {
                g.add(me, log, &json!({"log": log, "n": 0}).to_string());
            }
            g
        }

        fn add(&mut self, me: &Identity, log: usize, line: &str) {
            let h = lines::hash(line);
            self.origins.insert(h, Origin::new(me, &h, 1));
            self.logs[log].insert(h, line.into());
        }
    }

    async fn send_raw(tx: &mut SendStream, v: &serde_json::Value) -> Result<(), String> {
        let body = serde_json::to_vec(v).map_err(|e| e.to_string())?;
        tx.write_all(&(body.len() as u32).to_be_bytes()).await.map_err(|e| e.to_string())?;
        tx.write_all(&body).await.map_err(|e| e.to_string())
    }

    /// What a 0.3.0 node said after the hello, step by step (base 188c262 `exchange`): its goal ids
    /// (a duplicate and an invalid one among them), a full reconciliation of each valid goal's logs,
    /// and `Live` with its hive beats. Returns the hive lines it gave and the presence and directives
    /// it took in.
    async fn old_exchange(n: &Node, dialler: bool, tx: &mut SendStream, rx: &mut RecvStream, goal: &OldGoal) -> Result<(usize, usize, usize), String> {
        let home = n.home.as_path();
        let mine = members(home)?;
        let Msg::Membership { .. } = swap(dialler, tx, rx, Msg::Membership { docs: Membership::history(home) }).await? else { return Err("membership".into()) };
        n.stamp()?;
        let (origins, mut report) = (origin::load(home), Report::default());
        for file in SYNCED {
            n.reconcile(dialler, tx, rx, &home.join(file), &origins, &mine, &mut report).await?;
        }
        let ids = ["g1", "g1", "Bad Id"].map(String::from).to_vec();
        let Msg::Goals { ids: theirs } = swap(dialler, tx, rx, Msg::Goals { ids }).await? else { return Err("goals".into()) };
        assert!(theirs.is_empty(), "a new node names no goal");
        let mut gave = 0;
        for set in &goal.logs {
            let mut r = Recon::new(set);
            loop {
                let m = r.message();
                let Msg::Round { round } = swap(dialler, tx, rx, Msg::Round { round: m.clone() }).await? else { return Err("round".into()) };
                if r.absorb(&m, round)? {
                    break;
                }
            }
            let give = within_budget(signed(set, &goal.origins, r.give.iter()));
            gave += give.len();
            let Msg::Lines { lines } = swap(dialler, tx, rx, Msg::Lines { lines: give }).await? else { return Err("lines".into()) };
            assert!(lines.is_empty(), "a new node gives no hive line");
        }
        for m in [
            Msg::Offer { hashes: vec![] },
            Msg::Want { hashes: vec![] },
            Msg::Lines { lines: vec![] },
            Msg::Ask { commits: vec![] },
            Msg::Lines { lines: vec![] },
        ] {
            swap(dialler, tx, rx, m).await?;
        }
        let now = borg::learn::now_ms();
        let live = json!({"t": "live", "presence": live::outgoing(home, &n.me, now), "directives": steer::outgoing(home, now),
            "beats": [{"goal": "g1", "line": "{}", "key": n.me.public(), "sig": "00"}]});
        let theirs = if dialler {
            send_raw(tx, &live).await?;
            recv(rx).await?
        } else {
            let theirs = recv(rx).await?;
            send_raw(tx, &live).await?;
            theirs
        };
        let Msg::Live { presence, directives } = theirs else { return Err("live".into()) };
        let me = n.me.public();
        let presence = live::receive(home, &mine, &me, presence, now)?;
        Ok((gave, presence, steer::receive(home, &mine, &me, &directives, now).0))
    }

    async fn old_dial(n: &Node, target: &str, goal: &OldGoal) -> Result<(usize, usize, usize), String> {
        let t = Target::parse(target)?;
        let conn = n.endpoint.connect(t.endpoint_addr(&n.relay)?, ALPN).await.map_err(|e| e.to_string())?;
        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
        swap(true, &mut tx, &mut rx, Msg::Hello { collective: "work".into(), listen: None, relay: None }).await?;
        let got = old_exchange(n, true, &mut tx, &mut rx, goal).await;
        let _ = tx.finish();
        conn.close(0u32.into(), b"done");
        got
    }

    async fn old_serve(n: &Node, goal: &OldGoal) -> Result<(usize, usize, usize), String> {
        let conn = n.incoming().await.ok_or("closed")?.await.map_err(|e| e.to_string())?;
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(|e| e.to_string())?;
        let Msg::Hello { .. } = recv(&mut rx).await? else { return Err("hello".into()) };
        send(&mut tx, &Msg::Hello { collective: "work".into(), listen: None, relay: None }).await?;
        let got = old_exchange(n, false, &mut tx, &mut rx, goal).await;
        let _ = tx.finish();
        conn.closed().await;
        got
    }

    /// A 0.3.0 peer (it names hive goals, reconciles their logs and sends beats in `Live`) keeps
    /// syncing with this node in both roles: lessons, presence and directives arrive both ways, no
    /// hive file is written, and a hive line it gave once is not sent again when this node dials.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_0_3_0_peer_syncs_in_both_roles_and_its_hive_lines_come_once() {
        let (pa, pb) = team("legacy");
        let (a, b) = (home(&pa), home(&pb));
        let (alice_id, bob_id) = (Identity::load_or_create(&a).unwrap(), Identity::load_or_create(&b).unwrap());
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.invite(&alice_id, &bob_id.public(), "steer").unwrap();
        m.save(&a).unwrap();
        m.save(&b).unwrap();
        let (l1, l2) = (lesson("L1", 1, "lock order?"), lesson("L2", 1, "retry budget?"));
        write(&a, "lessons.jsonl", std::slice::from_ref(&l1));
        write(&b, "lessons.jsonl", std::slice::from_ref(&l2));
        let now = borg::learn::now_ms();
        for (h, p, who) in [(&a, &pa, "alice"), (&b, &pb, "bob")] {
            let ev = json!({"session_id": format!("{who}-s1"), "cwd": p.dir, "prompt": format!("{who} works")});
            borg::presence::write(h, &borg::presence::update(None, "work", "claude", "prompt", &ev, now, |_| (format!("{who}@x"), "repo".into()))).unwrap();
        }
        let old = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let new = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let (old_at, new_at) = (old.ticket(), new.ticket());
        let mut goal = OldGoal::new(&alice_id);

        // The new node dials: every hive line comes once and is dropped.
        let (old_got, new_got) = tokio::join!(old_serve(&old, &goal), new.sync(&old_at));
        let (new_got, (gave, presence, _)) = (new_got.unwrap(), old_got.unwrap());
        assert_eq!((gave, presence, new_got.presence), (LEGACY_GOAL_LOGS, 1, 1));
        assert!(holds(&a, "lessons.jsonl", &l2) && holds(&b, "lessons.jsonl", &l1), "lessons both ways");

        // The old node dials: directives both ways, and the listener's echo moves no hive line.
        for (h, id, who) in [(&a, &alice_id, "alice@x"), (&b, &bob_id, "bob@x")] {
            let d = crate::steer::sign(id, crate::steer::new("work", who, borg::steer::Target::default(), borg::steer::SAY, "run the tests", now));
            let held = std::fs::read_to_string(crate::steer::outbox(h)).unwrap_or_default();
            std::fs::write(crate::steer::outbox(h), format!("{held}{}\n", serde_json::to_string(&d).unwrap())).unwrap();
        }
        let (old_got, new_got) = tokio::join!(old_dial(&old, &new_at, &goal), new.accept());
        let (new_got, (gave, _, steered)) = (new_got.unwrap(), old_got.unwrap());
        assert_eq!((gave, steered, new_got.steered), (0, 1, 1));

        // The new node dials again: only the line added since comes.
        goal.add(&alice_id, 5, "{\"bundle\":\"new\"}");
        let (old_got, new_got) = tokio::join!(old_serve(&old, &goal), new.sync(&old_at));
        new_got.unwrap();
        assert_eq!(old_got.unwrap().0, 1, "what was given before is not sent again");
        assert!(!b.join("hive").exists(), "no hive file on the new node");
        old.close().await;
        new.close().await;
    }

    /// A revokes C; B, which has not heard yet, still takes a lesson C wrote after its revocation.
    /// B's next sync with A brings the new membership first: the late lesson is quarantined before
    /// any data moves and never reaches A, C's earlier lesson stays, and C is refused by B.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revocation_spreads_and_the_revoked_node_is_refused() {
        let p = crew("revoke", &["alice", "bob", "carol"]);
        let (a, b, c) = (home(&p[0]), home(&p[1]), home(&p[2]));
        let alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap();
        let carol = Node::bind(&p[2], LOCAL.parse().unwrap()).await.unwrap();
        let (ta, tb, tc) = (alice.ticket(), bob.ticket(), carol.ticket());

        let early = lesson("Lc1", 1, "carol's useful lesson?");
        write(&c, "lessons.jsonl", std::slice::from_ref(&early));
        let (s, r) = tokio::join!(bob.accept(), carol.sync(&tb));
        assert_eq!((s.unwrap().received, r.unwrap().sent), (1, 1));
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let mut m = Membership::load(&a).unwrap().unwrap();
        m.revoke(&Identity::load_or_create(&a).unwrap(), &carol.public(), borg::learn::now_ms()).unwrap();
        m.save(&a).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let late = lesson("Lc2", 1, "carol's lesson after her revocation?");
        append_lines(&c, "lessons.jsonl", std::slice::from_ref(&late));
        let (s, r) = tokio::join!(bob.accept(), carol.sync(&tb));
        assert_eq!((s.unwrap().received, r.unwrap().sent), (1, 1), "bob has not heard of the revocation yet");
        assert!(holds(&b, "lessons.jsonl", &late));

        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ta));
        let (served, synced) = (served.unwrap(), synced.unwrap());
        assert_eq!(Membership::load(&b).unwrap().unwrap().version, m.version, "bob learned the revocation from alice");
        assert_eq!(synced.quarantined, 1, "carol's late lesson is set aside before any data moves");
        assert!(!holds(&b, "lessons.jsonl", &late) && holds(&b.join("mesh"), "quarantine.jsonl", &late));
        assert_eq!(served.received, 1, "alice gets carol's earlier lesson only");
        assert!(holds(&a, "lessons.jsonl", &early) && !holds(&a, "lessons.jsonl", &late));

        let (served, synced) = tokio::join!(bob.accept(), carol.sync(&tb));
        assert!(served.unwrap_err().contains("not a member"), "bob turns carol away");
        assert!(synced.is_err());
        let err = bob.sync(&tc).await.unwrap_err();
        assert!(err.contains("not a member") && err.contains("refusing to connect"), "{err}");
        for n in [alice, bob, carol] {
            n.close().await;
        }
    }

    /// After days apart, two nodes holding 2000 common lessons and 5 new ones between them move
    /// exactly the 5 lines and a few dozen hashes, not the history.
    #[tokio::test(flavor = "multi_thread")]
    async fn catch_up_transfers_only_the_difference() {
        let (pa, pb) = team("catch-up");
        let (a, b) = (home(&pa), home(&pb));
        let common: Vec<String> = (0..2000).map(|i| lesson(&format!("L{i}"), 1, &format!("question {i}?"))).collect();
        write(&a, "lessons.jsonl", &common);
        write(&b, "lessons.jsonl", &common);
        append_lines(&a, "lessons.jsonl", &(0..3).map(|i| lesson(&format!("A{i}"), 1, "alice offline?")).collect::<Vec<_>>());
        append_lines(&b, "lessons.jsonl", &(0..2).map(|i| lesson(&format!("B{i}"), 1, "bob offline?")).collect::<Vec<_>>());
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let (served, synced) = (served.unwrap(), synced.unwrap());
        assert_eq!((synced.received, synced.sent, served.received, served.sent), (3, 2, 2, 3));
        let hashes = synced.hashes + served.hashes;
        let fps = synced.fps + served.fps;
        eprintln!("catch-up: 5 of 2005 lines moved, {hashes} hashes and {fps} fingerprints in {} rounds", synced.rounds);
        assert!(hashes <= 2 * crate::recon::LEAF * 5, "hashes ∝ difference, not history: {hashes}");
        assert!(fps <= 2 * (1 + 16 * 5 * synced.rounds), "{fps}");
        assert_eq!(lines::read(&a.join("lessons.jsonl")), lines::read(&b.join("lessons.jsonl")));
        alice.close().await;
        bob.close().await;
    }

    /// A reasoning excerpt goes from its author to exactly its 2 rendezvous replicas, and no
    /// member accepts another member's excerpt passed off as a replica.
    #[tokio::test(flavor = "multi_thread")]
    async fn reasoning_goes_to_two_replicas() {
        let p = crew("replicas", &["alice", "bob", "carol", "dave"]);
        let a = home(&p[0]);
        let excerpt = json!({"commit": "abc", "task": "fix the lock", "proposal": "take it in order"}).to_string();
        write(&a, REASONING, std::slice::from_ref(&excerpt));
        let alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        let m = Membership::load(&a).unwrap().unwrap();
        let chosen = replicas(&lines::hash(&excerpt), &m, &alice.public());
        assert_eq!(chosen.len(), 2);
        let mut holders = Vec::new();
        for q in &p[1..] {
            let n = Node::bind(q, LOCAL.parse().unwrap()).await.unwrap();
            let ticket = alice.ticket();
            let (served, synced) = tokio::join!(alice.accept(), n.sync(&ticket));
            served.unwrap();
            synced.unwrap();
            if holds(&home(q), REASONING, &excerpt) {
                holders.push(n.public());
            }
            n.close().await;
        }
        holders.sort();
        let mut want = chosen.clone();
        want.sort();
        assert_eq!(holders, want, "exactly the two replicas hold it");
        alice.close().await;
    }

    /// With the default bind (0.0.0.0) no ticket says 0.0.0.0, and the listener remembers the
    /// dialler at the IP it saw it come from, with the port it announced: it can dial back.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_node_bound_to_any_address_is_dialled_back_where_it_was_seen() {
        let (pa, pb) = team("any-addr");
        let alice = Node::bind(&pa, "0.0.0.0:0".parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, "0.0.0.0:0".parse().unwrap()).await.unwrap();
        let (pa_port, pb_port) = (alice.addr().unwrap().port(), bob.addr().unwrap().port());
        for t in alice.tickets() {
            assert!(!t.contains("@0.0.0.0:") && t.ends_with(&format!(":{pa_port}")), "{t}");
        }
        let ticket = format!("{}@127.0.0.1:{pa_port}", alice.public());
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        assert_eq!(peers::load(&home(&pa)).get(&bob.public()), Some(&format!("127.0.0.1:{pb_port}")), "bob is remembered where alice saw him");
        // And alice can dial bob back there.
        let known = peers::load(&home(&pa));
        let back = format!("{}@{}", bob.public(), known[&bob.public()]);
        let (served, synced) = tokio::join!(bob.accept(), alice.sync(&back));
        served.unwrap();
        synced.unwrap();
        // A one-shot dialler announces nothing: its port is gone a moment later.
        let quiet = Node::bind(&pb, "0.0.0.0:0".parse().unwrap()).await.unwrap().quiet();
        assert_eq!(quiet.listen(), None);
        assert_eq!(reachable("0.0.0.0:7878", Some("10.0.0.5:51000".parse().unwrap())), Some("10.0.0.5:7878".parse().unwrap()));
        assert_eq!(reachable("0.0.0.0:7878", None), None);
        for n in [alice, bob, quiet] {
            n.close().await;
        }
    }

    /// B already holds a lesson C wrote after its revocation (from C, before B knew). The admin
    /// then hands B the new membership with `join`: the lesson is quarantined right there, not at
    /// some later sync (which, B holding the newest version already, would find nothing newer).
    /// The same for a node whose membership file another process replaced: `enforce` catches it.
    #[tokio::test(flavor = "multi_thread")]
    async fn join_and_enforce_quarantine_what_the_new_membership_refuses() {
        let p = crew("join-q", &["alice", "bob", "carol", "dave"]);
        let (a, b, c, d) = (home(&p[0]), home(&p[1]), home(&p[2]), home(&p[3]));
        let bob = Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap();
        let carol = Node::bind(&p[2], LOCAL.parse().unwrap()).await.unwrap();
        let dave = Node::bind(&p[3], LOCAL.parse().unwrap()).await.unwrap();
        let early = lesson("Lc1", 1, "carol's useful lesson?");
        write(&c, "lessons.jsonl", std::slice::from_ref(&early));
        carol.stamp().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let (m, _) = origin::revoke(&a, &Identity::load_or_create(&a).unwrap(), &carol.public(), borg::learn::now_ms()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let late = lesson("Lc2", 1, "carol's lesson after her revocation?");
        append_lines(&c, "lessons.jsonl", std::slice::from_ref(&late));
        for (node, h) in [(&bob, &b), (&dave, &d)] {
            let t = node.ticket();
            let (s, r) = tokio::join!(node.accept(), carol.sync(&t));
            s.unwrap();
            r.unwrap();
            assert!(holds(h, "lessons.jsonl", &late) && holds(h, "lessons.jsonl", &early));
        }

        let handed = std::fs::read_to_string(crate::membership::path(&a)).unwrap();
        let (got, moved) = origin::join(&b, "work", &crate::membership::parse_docs(&handed).unwrap()).unwrap();
        assert_eq!((got.version, moved), (m.version, 1));
        assert!(!holds(&b, "lessons.jsonl", &late) && holds(&b, "lessons.jsonl", &early), "quarantined at join");
        assert!(origin::join(&b, "work", &crate::membership::parse_docs(&handed).unwrap()).is_err(), "nothing newer the second time");

        m.save(&d).unwrap(); // installed behind the daemon's back
        assert_eq!(dave.enforce().unwrap(), 1);
        assert_eq!(dave.enforce().unwrap(), 0, "once per membership");
        assert!(!holds(&d, "lessons.jsonl", &late));
        for n in [bob, carol, dave] {
            n.close().await;
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// The author hands an excerpt to its two replicas and goes offline; a member that is no
    /// replica asks one of them by commit and gets it back as a git note.
    #[tokio::test(flavor = "multi_thread")]
    async fn reasoning_is_fetched_from_a_replica_while_its_author_is_offline() {
        let p = crew("fetch", &["alice", "bob", "carol", "dave"]);
        let a = home(&p[0]);
        let repo = p[3].base.join("work");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["-c", "user.email=ann@example.com", "-c", "user.name=Ann", "commit", "-q", "--allow-empty", "-m", "take the lock first"]);
        let commit = git(&repo, &["rev-parse", "HEAD"]);
        let excerpt = json!({"kind": "reasoning", "commit": commit, "repo": "work", "session": "s-9", "task": "fix the race",
            "proposal": "take the lock before reading the flag", "episodes": [], "ms": 1})
        .to_string();
        write(&a, REASONING, std::slice::from_ref(&excerpt));
        let alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        let chosen = replicas(&lines::hash(&excerpt), &Membership::load(&a).unwrap().unwrap(), &alice.public());
        let mut others = Vec::new();
        for q in &p[1..] {
            let n = Node::bind(q, LOCAL.parse().unwrap()).await.unwrap();
            let t = alice.ticket();
            let (served, synced) = tokio::join!(alice.accept(), n.sync(&t));
            served.unwrap();
            synced.unwrap();
            others.push((q.clone(), n));
        }
        alice.close().await; // the author is offline from here on
        let (holders, askers): (Vec<_>, Vec<_>) = others.into_iter().partition(|(_, n)| chosen.contains(&n.public()));
        let (asker_place, asker) = askers.into_iter().next().expect("one member is no replica");
        let holder = &holders[0].1;
        assert!(!holds(&home(&asker_place), REASONING, &excerpt));

        let short = commit[..10].to_string();
        let t = holder.ticket();
        let (served, fetched) = tokio::join!(holder.accept(), asker.fetch(&t, std::slice::from_ref(&short)));
        assert_eq!(served.unwrap().answered, 1, "a replica answers for an excerpt it did not write");
        assert_eq!(fetched.unwrap().received, 1);
        let written = borg::notes::restore(&repo, &home(&asker_place), std::slice::from_ref(&commit)).unwrap();
        assert_eq!(written, vec![commit.clone()]);
        assert!(git(&repo, &["notes", "--ref=refs/notes/borg", "show", &commit]).contains("take the lock before reading the flag"));

        // Nobody is handed an excerpt it did not ask for.
        let (served, synced) = tokio::join!(holder.accept(), asker.sync(&t));
        assert_eq!((served.unwrap().answered, synced.unwrap().received), (0, 0));
        asker.close().await;
        for (_, n) in holders {
            n.close().await;
        }
    }

    /// Presence and steering ride the session: bob (role steer) sees alice's drone, pauses it, and
    /// alice's hook gets the signed directive; a relayed observer's directive is dropped and logged.
    #[tokio::test(flavor = "multi_thread")]
    async fn presence_and_signed_steering_travel_between_members() {
        let (pa, pb) = team("live");
        let (a, b) = (home(&pa), home(&pb));
        let (alice_id, bob_id) = (Identity::load_or_create(&a).unwrap(), Identity::load_or_create(&b).unwrap());
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.invite(&alice_id, &bob_id.public(), "steer").unwrap();
        m.save(&a).unwrap(); // every version goes into the history, or the chain breaks
        let observer = Identity::load_or_create(&a.join("observer")).unwrap();
        m.invite(&alice_id, &observer.public(), "observe").unwrap();
        m.save(&a).unwrap(); // bob adopts both versions at the first sync
        let now = borg::learn::now_ms();
        let ev = json!({"session_id": "alice-s1", "cwd": pa.dir, "prompt": "refactor the recon loop"});
        borg::presence::write(&a, &borg::presence::update(None, "work", "claude", "prompt", &ev, now, |_| ("alice@x".into(), "repo".into()))).unwrap();

        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        assert_eq!(synced.unwrap().presence, 1);
        let ds = borg::top::drones(&b, true, now);
        assert_eq!((ds.len(), ds[0].node.as_str(), ds[0].p.prompt.as_str()), (1, alice_id.public().as_str(), "refactor the recon loop"));
        let lessons = std::fs::read_to_string(b.join("lessons.jsonl")).unwrap_or_default();
        assert!(!lessons.contains("recon loop"), "never in lessons");

        // Bob pauses alice's drone by its designation. Alongside travel an observer's directive
        // (bob's own check removes it before it leaves) and one of bob's with its text altered
        // after signing (alice drops and logs it).
        let said = crate::steer::issue(&b, "work", true, "bob@x", "One", borg::steer::PAUSE, "", now).unwrap();
        assert!(said.starts_with("paused One of One (session alice-s1, alice@x)"), "{said}");
        assert!(borg::steer::read_inbox(&b).is_empty(), "not for bob's own drones");
        let by_observer = crate::steer::sign(&observer, crate::steer::new("work", "obs@x", borg::steer::Target::default(), borg::steer::SAY, "ship it", now));
        let mut tampered =
            crate::steer::sign(&bob_id, crate::steer::new("work", "bob@x", borg::steer::Target::default(), borg::steer::SAY, "run the tests", now));
        tampered.text = "force-push main".into();
        let held = std::fs::read_to_string(crate::steer::outbox(&b)).unwrap();
        let extra: String = [&by_observer, &tampered].iter().map(|d| format!("{}\n", serde_json::to_string(d).unwrap())).collect();
        std::fs::write(crate::steer::outbox(&b), format!("{held}{extra}")).unwrap();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        synced.unwrap();
        let served = served.unwrap();
        assert_eq!((served.steered, served.dropped), (1, 1));
        let inbox = borg::steer::read_inbox(&a);
        assert_eq!((inbox.len(), inbox[0].role.as_str(), inbox[0].d.engineer.as_str()), (1, "steer", "bob@x"));
        let log = std::fs::read_to_string(borg::steer::dropped_path(&a)).unwrap();
        assert!(log.contains("signature") && log.contains(&tampered.id), "{log}");
        assert!(!std::fs::read_to_string(crate::steer::outbox(&b)).unwrap().contains("ship it"), "an observer's directive is not relayed");
        // Alice's hook refuses tools until bob resumes.
        let pre = json!({"session_id": "alice-s1", "cwd": pa.dir, "tool_name": "Bash"});
        let deny = borg::steer::around(&a, true, "pre_tool", &pre, Some(now), now + 1, || None).unwrap();
        assert!(
            deny["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains(&format!("paused by bob@x [key {}] (signed, role steer)", &bob_id.public()[..12]))
        );
        crate::steer::issue(&b, "work", true, "bob@x", "alice-s1", borg::steer::RESUME, "", now + 2).unwrap();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        synced.unwrap();
        assert_eq!(served.unwrap().steered, 1);
        assert_eq!(borg::steer::around(&a, true, "pre_tool", &pre, Some(now), now + 3, || None), None);

        // Bob is revoked: his directives stop holding at alice.
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let _ = (served.unwrap(), synced.unwrap());
        crate::steer::issue(&b, "work", true, "bob@x", "One", borg::steer::PAUSE, "", now + 4).unwrap();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let _ = (served.unwrap(), synced.unwrap());
        assert!(borg::steer::around(&a, true, "pre_tool", &pre, Some(now), now + 5, || None).is_some(), "paused again");
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.revoke(&alice_id, &bob_id.public(), now + 5).unwrap();
        m.save(&a).unwrap();
        alice.enforce().unwrap();
        assert!(borg::steer::read_inbox(&a).is_empty(), "the revoked signer's directives are gone");
        assert_eq!(borg::steer::around(&a, true, "pre_tool", &pre, Some(now), now + 6, || None), None, "and the pause no longer holds");
        alice.close().await;
        bob.close().await;
    }

    /// `borg no` on bob's node for alice's drone: signed, carried by the round's steering, recorded once on
    /// alice's node (credit, episode, stat), shown to her drone once, and back on bob's node as a team
    /// outcome, so both nodes' beliefs count the veto once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_veto_on_one_node_reaches_the_drones_node_once() {
        let (pa, pb) = team("veto");
        let (a, b) = (home(&pa), home(&pb));
        let (alice_id, bob_id) = (Identity::load_or_create(&a).unwrap(), Identity::load_or_create(&b).unwrap());
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.invite(&alice_id, &bob_id.public(), "steer").unwrap();
        m.save(&a).unwrap();
        let now = borg::learn::now_ms();
        let ev = json!({"session_id": "alice-s1", "cwd": pa.dir, "prompt": "look merges up"});
        borg::presence::write(&a, &borg::presence::update(None, "work", "claude", "prompt", &ev, now, |_| ("alice@x".into(), "repo".into()))).unwrap();
        write(&a, "lessons.jsonl", &[lesson("L1", 1, "how are merges found?")]);
        let asked = json!({"kind": "asked", "session": "alice-s1", "lessons": ["L1"], "ms": now - 10, "repo": "repo"}).to_string();
        write(&a, "outcomes.jsonl", &[asked]);
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let _ = (served.unwrap(), synced.unwrap());

        let said = crate::steer::issue(&b, "work", true, "bob@x", "One", borg::steer::VETO, "never look merges up in the commit log", now).unwrap();
        assert!(said.starts_with("vetoed One of One (session alice-s1, alice@x)"), "{said}");
        assert!(crate::steer::issue(&b, "work", true, "bob@x", "all", borg::steer::VETO, "no", now).is_err(), "a veto names one drone");
        assert!(!b.join("episodes.jsonl").exists(), "the issuing node records no veto of its own");
        for _ in 0..2 {
            let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
            let _ = (served.unwrap(), synced.unwrap());
        }
        let vetoed =
            |h: &Path| borg::outcome::read(&h.join("outcomes.jsonl")).into_iter().filter(|o| o.kind == borg::outcome::VETOED && o.lessons == ["L1"]).count();
        assert_eq!(vetoed(&a), 1, "credited once on the drone's node, over two rounds");
        let eps = std::fs::read_to_string(a.join("episodes.jsonl")).unwrap();
        assert_eq!(eps.lines().count(), 1);
        assert!(eps.contains("\"kind\":\"veto\"") && eps.contains("bob@x") && eps.contains("never look merges up"), "{eps}");
        let stats: Vec<_> = borg::stats::events(&a).into_iter().filter(|e| e.kind == borg::stats::VETO).collect();
        assert_eq!((stats.len(), stats[0].why.as_str()), (1, "mesh"));
        // Alice's drone is told once, labelled with bob's key.
        let prompt = json!({"session_id": "alice-s1", "cwd": pa.dir});
        let out = borg::steer::around(&a, true, "prompt", &prompt, Some(now - 10), now + 1, || None).unwrap();
        let ctx = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.starts_with(&format!("[borg] Veto from bob@x [key {}] (signed, role steer): The engineer said no", &bob_id.public()[..12])), "{ctx}");
        assert_eq!(borg::steer::around(&a, true, "prompt", &prompt, Some(now - 10), now + 2, || None), None);
        // Both nodes' beliefs count it once: alice's from her own record, bob's from the team outcome.
        let l1 = [borg::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let belief = |h: &Path| borg::memory::derive(&borg::outcome::read_team(&h.join("lessons.jsonl")), &l1, None)["L1"].vetoed;
        assert_eq!((belief(&a), belief(&b)), (1, 1));
        assert!(!b.join("outcomes.jsonl").exists() && !b.join("episodes.jsonl").exists(), "bob's node holds only the team copy");
        alice.close().await;
        bob.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_collectives_presence_and_directives_never_leave() {
        let p = node("live-private", "local");
        let h = home(&p);
        let now = borg::learn::now_ms();
        let ev = json!({"session_id": "s1", "cwd": p.dir, "prompt": "private diary"});
        borg::presence::write(&h, &borg::presence::update(None, "work", "claude", "prompt", &ev, now, |_| ("me@x".into(), "r".into()))).unwrap();
        let said = crate::steer::issue(&h, "work", false, "me@x", "One", borg::steer::SAY, "lunch", now).unwrap();
        assert!(said.contains("never leaves this machine"), "{said}");
        assert_eq!(borg::steer::read_inbox(&h)[0].role, "local");
        assert!(!crate::steer::outbox(&h).exists(), "nothing queued for the mesh");
        assert!(Node::bind(&p, LOCAL.parse().unwrap()).await.is_err(), "no socket at all");
    }

    /// The admin answers every dial-in (joins and syncs) until the test ends.
    fn answer(n: std::sync::Arc<Node>) {
        tokio::spawn(async move {
            while let Some(i) = n.incoming().await {
                let n = n.clone();
                tokio::spawn(async move {
                    let _ = n.handle(i).await;
                });
            }
        });
    }

    fn ticket(admin: &Node, role: &str, ttl_ms: i64, uses: u32) -> Ticket {
        let home = admin.home();
        let m = Membership::load(home).unwrap().unwrap();
        let invite = invite::issue(home, &admin.me, &m, role, ttl_ms, uses, borg::learn::now_ms()).unwrap();
        Ticket::decode(&Ticket { invite, relay: admin.relay.clone(), hints: admin.hints() }.encode()).unwrap()
    }

    /// Bob and carol join alice's collective with tickets alone (no key exchanged by hand, no file
    /// handed over): each gets the signed membership with the ticket's role and alice's lesson at
    /// once. A spent ticket is refused for carol but welcomes bob again; an expired one is refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn three_nodes_join_by_ticket_and_sync_a_lesson() {
        let pa = crew("ticket", &["alice"]).remove(0);
        let (pb, pc) = (node("ticket-bob", "team"), node("ticket-carol", "team"));
        let l1 = lesson("L1", 1, "who owns the lock?");
        write(&home(&pa), "lessons.jsonl", std::slice::from_ref(&l1));
        let alice = std::sync::Arc::new(Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap());
        answer(alice.clone());

        let one = ticket(&alice, "member", 60_000, 1);
        let bob = Node::joining(&pb, LOCAL.parse().unwrap()).await.unwrap().quiet();
        let (m, rep) = bob.join(&one).await.unwrap();
        assert_eq!(m.role_of(&bob.public()), Some("member"));
        assert_eq!(rep.received, 1, "bob syncs at once");
        assert!(holds(&home(&pb), "lessons.jsonl", &l1));
        assert_eq!(peers::load(&home(&pb)).get(&alice.public()), Some(&addr_of(&alice)), "bob knows where alice is");
        assert!(peers::load(&home(&pa)).contains_key(&bob.public()), "alice knows bob, by key at least");

        let carol = Node::joining(&pc, LOCAL.parse().unwrap()).await.unwrap().quiet();
        let err = carol.join(&one).await.unwrap_err();
        assert!(err.contains("already used"), "{err}");
        assert!(Membership::load(&home(&pc)).unwrap().is_none() && !home(&pc).join("lessons.jsonl").exists(), "carol got nothing");
        assert!(bob.join(&one).await.is_ok(), "bob again (a lost answer): welcomed");

        let gone = ticket(&alice, "member", 1, 1);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(carol.join(&gone).await.unwrap_err().contains("expired"));

        let two = ticket(&alice, "observe", 60_000, 1);
        let (m, rep) = carol.join(&two).await.unwrap();
        assert_eq!(m.role_of(&carol.public()), Some("observe"));
        assert_eq!(rep.received, 1);
        let held = Membership::load(&home(&pa)).unwrap().unwrap();
        assert_eq!((held.role_of(&bob.public()), held.role_of(&carol.public())), (Some("member"), Some("observe")));
        assert!(invite::pending(alice.home(), borg::learn::now_ms()).is_empty(), "both tickets spent");

        // Bob learns of carol at his next sync with alice: the signed chain spreads as before.
        assert_eq!(Membership::load(&home(&pb)).unwrap().unwrap().role_of(&carol.public()), None);
        bob.sync(&format!("{}@{}", alice.public(), addr_of(&alice))).await.unwrap();
        assert_eq!(Membership::load(&home(&pb)).unwrap().unwrap().role_of(&carol.public()), Some("observe"));
        for n in [bob, carol] {
            n.close().await;
        }
        alice.close().await;
    }

    /// The NAT case as far as one machine can show it: neither node has a direct address (no UDP
    /// socket at all), the ticket carries none, so the join and the sync can only go through the
    /// (local, self-signed) relay both use.
    #[tokio::test(flavor = "multi_thread")]
    async fn join_and_sync_go_through_the_relay_when_no_direct_address_is_known() {
        let (_map, url, _server) = iroh::test_utils::run_relay_server().await.unwrap();
        let pa = crew("relay", &["alice"]).remove(0);
        let pb = node("relay-bob", "team");
        for p in [&pa, &pb] {
            let cfg = json!({"default": "off", "collectives": [{"name": "work", "paths": [p.dir], "share": "team", "relay": url.to_string()}]});
            std::fs::write(&p.config, cfg.to_string()).unwrap();
        }
        let l1 = lesson("L1", 1, "does this cross the NAT?");
        write(&home(&pa), "lessons.jsonl", std::slice::from_ref(&l1));
        let trust = |b: Builder| b.ca_tls_config(iroh::tls::CaTlsConfig::insecure_skip_verify());
        let alice = std::sync::Arc::new(Node::open(&pa, None, true, trust).await.unwrap());
        let bob = Node::open(&pb, None, false, trust).await.unwrap().quiet();
        assert!(matches!(alice.relay(), Relay::Custom(_)));
        assert!(alice.tickets().is_empty() && bob.tickets().is_empty(), "no direct address on either side");
        let wait = std::time::Duration::from_secs(10);
        assert!(alice.online(wait).await && bob.online(wait).await, "both reach the relay");
        answer(alice.clone());

        let mut t = ticket(&alice, "member", 60_000, 1);
        t.hints.retain(|h| !matches!(h, Hint::Direct(_)));
        let (m, rep) = bob.join(&t).await.unwrap();
        assert_eq!(m.role_of(&bob.public()), Some("member"));
        assert_eq!(rep.received, 1);
        assert!(holds(&home(&pb), "lessons.jsonl", &l1), "the lesson crossed through the relay");
        // And a later sync by key alone: the configured relay is the only way.
        let l2 = lesson("L2", 1, "and back?");
        append_lines(&home(&pb), "lessons.jsonl", std::slice::from_ref(&l2));
        assert_eq!(bob.sync(&alice.public()).await.unwrap().sent, 1);
        assert!(holds(&home(&pa), "lessons.jsonl", &l2));
        bob.close().await;
        alice.close().await;
    }
}
