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
//! 3. `lessons.jsonl`, `stats.jsonl`, `outcomes-team.jsonl`, `playbooks.jsonl`, `constraints.jsonl`, `messages.jsonl`,
//!    `absorbed.jsonl` (lines.rs `SYNCED` and the removed playbooks' step, `PLAYBOOKS`; a 0.4.0 to 0.6.x
//!    peer's `borg/mesh/13` or `/14` session stops before the absorbs, a 0.3.0 peer's `borg/mesh/12`
//!    session before the messages), each in turn:
//!    range-based set reconciliation (recon.rs), cost ∝ difference: one fingerprint when nothing
//!    changed. Then `Lines`: what the other side lacks, each line with its
//!    signed origin (origin.rs). A line is stored only if the peer announced it, its origin verifies,
//!    the membership admits its origin key at its stamp time, and it may travel under that key
//!    (lines.rs `travels`: a message or seen line names its origin key as its node, and is fresh; an
//!    absorb or release has the shape borg's write path gives it).
//!    Messages also travel only within their `window` (borg msg.rs `within_windows`: what any sender's
//!    window, msg.rs `window_for`, can have let through to one recipient, decided by no earlier line, so
//!    pruning never takes a held line out of it), on offer and on receipt, and the log stays bounded as
//!    `send` bounds it (msg.rs `bound`, bytes per member and class): a new line the bound would cut is
//!    refused, so it is not taken again. What the log takes is decided before its lock and written
//!    under it (`stored`, `Store::store_messages`), so a hook's acknowledgement never waits on it. A
//!    `borg/mesh/13` peer is offered only what 0.5.0 takes (`v1_window`).
//!    A pause, resume or veto from another node counts only once admitted under its node's role
//!    (steer.rs `admit`), when it is stored.
//!
//!    The playbooks are removed, but their step is still spoken so a 0.3.0 or 0.4.0 peer keeps its
//!    order: this node offers nothing there, follows the peer's reconciliation and stores nothing
//!    (`skip`), as for a hive goal below.
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
//!    latest per member and session, a day at most). A 0.3.0 peer's hive beats and signed steering
//!    directives in it are ignored: steering travels as messages, admitted when stored (steer.rs).
//!
//! A completed session, dialled or served, is recorded on each side by when it started, per member
//! (`mesh/synced.json`, borg presence.rs `synced_path`), and by what its protocol says the member reads
//! (`reads`, `mesh/reads.json`): `send` refuses a text the member's sessions could never be shown.
//!
//! Addressing is by key (addr.rs): a peer is dialled at its node id, with direct addresses and
//! relay URLs as optional hints. How the endpoint reaches it across NAT is the collective's relay
//! setting: n0's relays and address lookup (default), a self-hosted relay, or direct only.
//!
//! **Joining** (invite.rs) is a second ALPN: a node that is not a member yet presents a signed
//! invite to the admin that issued it, and gets the membership history back (`join`, `admit`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use borg::collective::{self, Config, Route};
use borg::msg::{Line, V1_RATE_MS};
use iroh::Endpoint;
use iroh::endpoint::{Builder, ConnectOptions, Connection, ConnectionError, Incoming, IncomingAddr, PortmapperConfig, RecvStream, SendStream, presets};
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
use borg::protocol::PeerVersion;

mod vectors;

/// The session protocol. 15: `absorbed.jsonl` joined the synced logs, last (lines.rs `ABSORBED`). 14:
/// messages v2 (borg msg.rs: texts kept whole up to `WIRE_TEXT`, `seen` lines, the acknowledgement
/// window instead of 0.5.0's rate). 13: `messages.jsonl` joined the synced logs (lines.rs `SYNCED`).
/// The logs reconcile in a fixed list, so a peer on another list would reconcile another file at the
/// same step; 11 added `constraints.jsonl` and refused 10 at the handshake for that reason.
pub const ALPN: &[u8] = b"borg/mesh/15";
/// 0.6.x's protocol, still spoken whole: the session of `ALPN` without the absorbs (`synced`). Such a
/// peer reads every message line this node writes.
pub const V14_ALPN: &[u8] = b"borg/mesh/14";
/// 0.4.0 and 0.5.x's protocol, still spoken whole: `V14_ALPN`'s steps, but such a peer reads only v1
/// message lines and keeps only what 0.5.0's window takes (`V1_RATE_MS`, `V1_KEEP`), so it is offered
/// only that (`v1_window`): no v2 text, no seen line, one tell or ask per sender and recipient per
/// rate slot.
pub const V13_ALPN: &[u8] = b"borg/mesh/13";
/// 0.3.0's protocol, still spoken whole: the session of `V13_ALPN` without the messages (`synced`). 12
/// already outlived the hive: its steps (`Goals`, the beats of `Live`) are spoken empty.
pub const LEGACY_ALPN: &[u8] = b"borg/mesh/12";
/// The protocols of older nodes (`borg/mesh/11` down to `/1`), offered and accepted beside `ALPN` only to
/// tell a mismatch: without them the handshake fails and nothing says why. A session on one is closed at
/// once and the peer recorded (`mismatch`). A newer node does the same with this one's `ALPN`.
fn older() -> Vec<Vec<u8>> {
    let below = version(LEGACY_ALPN).unwrap_or(0);
    (1..below).rev().map(|v| format!("borg/mesh/{v}").into_bytes()).collect()
}

/// Protocol: the close code of a session on a protocol this node does not speak (`mismatch`), with the
/// reason borg protocol.rs `Mismatch::told`. The node on the other end reads both (`refused`): a newer
/// member that no longer speaks this node's protocol agrees on it at the handshake and then refuses it.
const MISMATCH_CLOSE: u32 = 2;

/// The other side of a session, as the handshake left it: its member key and the protocol it speaks.
#[derive(Clone, Copy)]
struct Peer<'a> {
    key: &'a str,
    alpn: &'a [u8],
}

/// Whether a session on `alpn` syncs with this node: `ALPN`, a 0.6.x peer's `V14_ALPN`, a 0.4.0 or 0.5.x
/// peer's `V13_ALPN`, or a 0.3.0 peer's `LEGACY_ALPN`.
fn spoken(alpn: &[u8]) -> bool {
    alpn == ALPN || alpn == V14_ALPN || alpn == V13_ALPN || alpn == LEGACY_ALPN
}

/// The removed playbooks' log. Its step (after the outcomes) stays in the session, so `ALPN` and
/// `LEGACY_ALPN` keep their order; a peer that still reconciles it is followed and nothing is kept
/// (`skip`). A leftover file in a store is neither offered nor stamped.
const PLAYBOOKS: &str = "playbooks.jsonl";

/// The logs a session on `ALPN` steps through, in order: `SYNCED` with the `PLAYBOOKS` step.
const STEPS: [&str; 7] = [SYNCED[0], SYNCED[1], SYNCED[2], PLAYBOOKS, SYNCED[3], SYNCED[4], SYNCED[5]];

/// The logs a session on `alpn` steps through, in order: all of `STEPS` on `ALPN`; without the absorbs
/// the six a 0.4.0 to 0.6.x peer knows (`V13_ALPN`, `V14_ALPN`); without the messages too on
/// `LEGACY_ALPN` the five a 0.3.0 peer knows.
fn synced(alpn: &[u8]) -> impl Iterator<Item = &'static str> {
    let (absorbs, messages) = (alpn == ALPN, alpn != LEGACY_ALPN);
    STEPS.into_iter().filter(move |f| (absorbs || *f != lines::ABSORBED) && (messages || *f != borg::msg::FILE))
}

/// The number of a session protocol (`borg/mesh/<n>`).
fn version(alpn: &[u8]) -> Option<u32> {
    std::str::from_utf8(alpn).ok()?.strip_prefix("borg/mesh/")?.parse().ok()
}
/// Protocol: the newest message line format (borg msg.rs `Message::v`) a peer whose session ran on
/// `alpn` reads: 2 on `ALPN` and `V14_ALPN`; 1 on `V13_ALPN`, whose 0.5.0 drops a v2 line; 0 on
/// `LEGACY_ALPN`, which syncs no messages (`synced`).
fn reads(alpn: &[u8]) -> u8 {
    if alpn == ALPN || alpn == V14_ALPN {
        2
    } else if alpn == V13_ALPN {
        1
    } else {
        0
    }
}
/// Protocol: the join handshake, an invite for the membership history (`Node::join`, `Node::admit`).
pub const JOIN_ALPN: &[u8] = b"borg/join/1";
/// Guard: the largest frame accepted; a lessons log is far smaller, a bigger frame is a broken or
/// hostile peer, which would otherwise make this node allocate what it claims.
const MAX_FRAME: usize = 256 << 20;
/// Guard: the most bytes of lines one `Lines` frame gives (well within `MAX_FRAME`): a big difference
/// goes over several sessions, never in a frame the peer refuses.
const MAX_GIVE: usize = 64 << 20;
/// Baseline: how many members hold a replica of each reasoning excerpt besides its author (two, so an
/// excerpt outlives one member going away; design-chosen).
pub const REPLICAS: usize = 2;
/// Guard: the most commits one `Ask` names, and most excerpts one answer carries, so one round of a
/// long history cannot make a peer read every excerpt it holds.
const MAX_ASK: usize = 1000;
/// Protocol: how many logs per goal a 0.3.0 peer reconciles in step 3b (its hive's synced logs: goal, tasks,
/// contracts, messages, flags, bundles, spend).
const LEGACY_GOAL_LOGS: usize = 7;
// 0.5.0's message pacing is borg msg.rs `V1_RATE_MS` (protocol: its `RATE_MS`), which a `V13_ALPN` peer
// applies to what it holds and receives: one tell or ask per sender and recipient that often. It is all
// such a peer takes, so it is all it is offered (`v1_window`); this node's `send` paces by msg.rs
// `window_for`, and the mesh bounds by msg.rs `within_windows`.
/// Protocol: 0.5.0's line bound (its msg.rs `KEEP`), which a `V13_ALPN` peer shares equally among the
/// members (`v1_share`) and applies on receipt, as `V1_RATE_MS`.
const V1_KEEP: usize = 500;

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
    /// Another node's pauses, resumes and vetoes newly admitted / refused (steer.rs).
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
    /// older nodes). `borg`: the sender's borg version (borg protocol.rs `PeerVersion`), absent from a
    /// node older than 0.7, which ignores it in a hello it reads.
    Hello {
        collective: String,
        listen: Option<String>,
        #[serde(default)]
        relay: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        borg: Option<String>,
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
        #[serde(default)]
        directives: Directives,
    },
}

/// 0.3.0's signed steering directives in `Live`: sent empty, as a 0.3.0 peer requires the field, and
/// a 0.3.0 peer's ignored unread. Steering travels as messages now (steer.rs).
#[derive(Debug, Default)]
struct Directives;

impl Serialize for Directives {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(std::iter::empty::<()>())
    }
}

impl<'de> Deserialize<'de> for Directives {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(d).map(|_| Directives)
    }
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
    /// The hashes each peer gave per log this node follows and keeps nowhere (`skip`: the playbooks, or
    /// a 0.3.0 peer's hive goal log, 3b), by (peer, goal or file, log): never stored, so the next dial
    /// reconciles against them and the peer gives only what is new.
    legacy: Mutex<HashMap<(String, String, usize), BTreeSet<Hash>>>,
    /// What this node says it is and announces against (`Speaks`).
    speaks: Speaks,
    /// Held from `announce`'s check for a held announcement to its send: sessions end concurrently
    /// under `serve`, and each would otherwise find none and send its own.
    announcing: Mutex<()>,
}

/// What a node tells its peers it is: the newest protocol it speaks and its borg version, and whether it
/// announces a session on an older protocol (`Node::announce`). This build's: `ALPN`, its
/// `CARGO_PKG_VERSION`, and `BORG_ANNOUNCE` not `off`. A test poses as an older node by changing it.
#[derive(Debug, Clone, Copy)]
struct Speaks {
    alpn: &'static [u8],
    /// Sent in the hello; `None` as a node older than 0.7 sends none.
    borg: Option<&'static str>,
    tell: bool,
}

impl Speaks {
    fn this() -> Speaks {
        Speaks { alpn: ALPN, borg: Some(env!("CARGO_PKG_VERSION")), tell: tells(std::env::var("BORG_ANNOUNCE").ok().as_deref()) }
    }
}

/// Whether a node announces, given `BORG_ANNOUNCE`: unless it is `off`.
fn tells(var: Option<&str>) -> bool {
    var != Some("off")
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
            .alpns([vec![ALPN.to_vec(), JOIN_ALPN.to_vec(), V14_ALPN.to_vec(), V13_ALPN.to_vec(), LEGACY_ALPN.to_vec()], older()].concat())
            .clear_ip_transports()
            .ca_tls_config(ca_trust());
        if let Some(a) = addr {
            builder = builder.bind_addr(a).map_err(|e| e.to_string())?;
        }
        let endpoint = tweak(builder).bind().await.map_err(|e| format!("bind {}: {e}", addr.map(|a| a.to_string()).unwrap_or("relay".into())))?;
        Ok(Node {
            place: place.clone(),
            home,
            me,
            endpoint,
            store: Mutex::new(()),
            announce: true,
            relay,
            legacy: Mutex::default(),
            speaks: Speaks::this(),
            announcing: Mutex::new(()),
        })
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

    /// Stamps the lines this node's hooks wrote since the last call as its own (`Store::stamp`).
    pub fn stamp(&self) -> Result<usize, String> {
        self.local().stamp()
    }

    /// This node's store, as a merge writes it (`Store`).
    fn local(&self) -> Store<'_> {
        Store { home: &self.home, lock: &self.store, me: &self.me }
    }

    /// Records a completed session with `remote` on `alpn` that started at `started` (borg presence.rs
    /// `synced_path`): a claim written before it reached the peer, and the peer's earlier claims came
    /// back. And what that node reads (`reads`, presence.rs `reads_path`), so `send` refuses a message
    /// its sessions could never be shown, and the version its hello gave (`borg`, borg protocol.rs
    /// `versions_path`) with its engineer as its drones name them. Then, with the store's lock released,
    /// the announcement of this node's protocol, if the session agreed on an older one (`announce`).
    fn synced(&self, remote: &str, alpn: &[u8], started: i64, borg: Option<String>) {
        {
            let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let _ = borg::presence::note_synced(&self.home, remote, started);
            let _ = borg::presence::forget_mismatch(&self.home, remote);
            let _ = borg::presence::note_reads(&self.home, remote, reads(alpn));
            if let Some(protocol) = version(alpn) {
                let now = borg::learn::now_ms();
                let engineer = engineer_of(&self.home, remote, now);
                let v = PeerVersion { borg, protocol, ms: now, engineer };
                let _ = borg::protocol::note_version(&self.home, remote, &v);
            }
        }
        self.announce(alpn);
    }

    /// Tells the collective that this node's protocol is in use (borg protocol.rs `announcement`) after
    /// a session agreed on the older `alpn`, as a tell to all from this node's engineer: the peer still
    /// syncs messages, as does every node since 0.4. Once: not while the message log holds one
    /// (`announced`; one session at a time checks and sends, `announcing`). Not while the engineer's
    /// slot to all on a 0.4 or 0.5 member holds another tell (`v1_slot_held`): that member is offered
    /// the earlier one alone. A send refused either way, or by the window, is dropped, and the next such
    /// session tries again. `None` when nothing was sent.
    fn announce(&self, alpn: &[u8]) -> Option<borg::msg::Message> {
        let (ours, theirs) = (version(self.speaks.alpn)?, version(alpn)?);
        if !self.speaks.tell || theirs >= ours {
            return None;
        }
        let _one = self.announcing.lock().unwrap_or_else(|e| e.into_inner());
        if borg::protocol::announced(&self.home, ours) {
            return None;
        }
        // As msg.rs `sender` names the engineer of a repo; never a session the mesh process's
        // environment may name.
        let email: String = borg::learn::engineer(&self.place.dir).chars().filter(|c| c.is_ascii_alphanumeric() || "-_.@+".contains(*c)).collect();
        let (by, who) = match email.as_str() {
            "" => (borg::msg::ENGINEER.to_string(), format!("the engineer of node {}", borg::msg::short_node(&self.me.public()))),
            e => (format!("{}:{e}", borg::msg::ENGINEER), e.to_string()),
        };
        let name = if email.is_empty() { borg::msg::ENGINEER.to_string() } else { email };
        let from = borg::msg::Sender { by, name, harness: "terminal".into(), node: self.me.public() };
        let now = borg::learn::now_ms();
        if v1_slot_held(&borg::msg::read(&self.home), &from, now) {
            return None;
        }
        let text = borg::protocol::announcement(ours, theirs, &who, self.speaks.borg.unwrap_or("older than 0.7"));
        borg::msg::send(&self.home, borg::msg::Message::new(borg::msg::TELL, &from, borg::msg::ALL, None, now, &text), now).ok()
    }

    /// A session with member `remote` that came up on another protocol (`older`): recorded for `borg
    /// doctor` (borg protocol.rs `protocols_path`), closed, and the error says who has to upgrade; the
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
        conn.close(MISMATCH_CLOSE.into(), told.as_bytes());
        line
    }

    /// Whether member `remote` closed this session because it does not speak the protocol agreed on
    /// (`MISMATCH_CLOSE`, as its `mismatch` does): a newer node that dropped this node's protocol. Then
    /// it is recorded as `mismatch` records an older one, from the close reason, so `borg doctor`, the
    /// views and the drone notice say that this node has to upgrade; the line says so too.
    fn refused(&self, conn: &Connection, remote: &str) -> Option<String> {
        let Some(ConnectionError::ApplicationClosed(c)) = conn.close_reason() else { return None };
        if u64::from(c.error_code) != u64::from(MISMATCH_CLOSE) {
            return None;
        }
        let said = borg::presence::Mismatch::from_told(&String::from_utf8_lossy(&c.reason), borg::learn::now_ms())?;
        let ours = version(ALPN)?;
        if borg::protocol::number(&said.theirs)? <= ours {
            return None;
        }
        let m = borg::presence::Mismatch { ours: String::from_utf8_lossy(ALPN).into_owned(), ..said };
        let line = m.line(remote);
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let _ = borg::presence::note_mismatch(&self.home, remote, m);
        Some(line)
    }

    /// Forgets `remote`'s last completed session (presence.rs `forget_synced`): a dial to it failed.
    pub(crate) fn unreachable(&self, remote: &str) {
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let _ = borg::presence::forget_synced(&self.home, remote);
    }

    /// The borg version this node's hello gives (`Speaks`).
    fn borg(&self) -> Option<String> {
        self.speaks.borg.map(str::to_string)
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
        self.dial(target, commits, ALPN, [vec![V14_ALPN.to_vec(), V13_ALPN.to_vec(), LEGACY_ALPN.to_vec()], older()].concat()).await
    }

    /// `fetch` on the protocol `alpn`, with `also` offered beside it (a test speaks an older one alone).
    async fn dial(&self, target: &str, commits: &[String], alpn: &[u8], also: Vec<Vec<u8>>) -> Result<Report, String> {
        let (name, home) = self.gate()?;
        let (name, home) = (name.as_str(), home.as_path());
        let membership = members(home)?;
        let t = Target::parse(target)?;
        if membership.role_of(&t.key).is_none() {
            return Err(format!("{} is not a member of {name}: refusing to connect", t.key));
        }
        let started = borg::learn::now_ms();
        let connecting = self.endpoint.connect_with_opts(t.endpoint_addr(&self.relay)?, alpn, ConnectOptions::new().with_additional_alpns(also));
        let conn = connecting.await.map_err(|e| format!("connect {target}: {e}"))?.await.map_err(|e| format!("connect {target}: {e}"))?;
        if !spoken(conn.alpn()) {
            return Err(self.mismatch(&conn, &t.key));
        }
        let result = async {
            let remote = authorise(&conn, &membership, name)?;
            let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
            let hello = Msg::Hello { collective: name.into(), listen: self.listen(), relay: self.announced(), borg: self.borg() };
            let Msg::Hello { collective, borg, .. } = swap(true, &mut tx, &mut rx, hello).await? else {
                return Err("protocol: expected hello".into());
            };
            if collective != name {
                return Err(format!("peer answered for collective {collective:?}, not {name:?}"));
            }
            let report = self.exchange(Peer { key: &remote, alpn: conn.alpn() }, true, &mut tx, &mut rx, membership, commits).await?;
            let _ = tx.finish();
            self.synced(&remote, conn.alpn(), started, borg);
            Ok(report)
        }
        .await;
        let result = result.map_err(|e| self.refused(&conn, &t.key).unwrap_or(e));
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
        if !spoken(conn.alpn()) && conn.alpn() != JOIN_ALPN {
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
            let Msg::Hello { collective, listen, relay, borg } = recv(&mut rx).await? else { return Err("protocol: expected hello".into()) };
            if collective != name {
                let reason = format!("this node serves collective {name:?}, not {collective:?}");
                send(&mut tx, &Msg::Refused { reason: reason.clone() }).await?;
                return Err(reason);
            }
            heard(home, &remote, listen, relay, observed);
            send(&mut tx, &Msg::Hello { collective: name.into(), listen: self.listen(), relay: self.announced(), borg: self.borg() }).await?;
            let report = self.exchange(Peer { key: &remote, alpn: conn.alpn() }, false, &mut tx, &mut rx, membership, &[]).await?;
            let _ = tx.finish();
            conn.closed().await; // the dialler closes once it has read everything
            self.synced(&remote, conn.alpn(), started, borg);
            Ok(report)
        };
        let result = if conn.alpn() == JOIN_ALPN {
            self.admit(&conn, observed).await
        } else {
            session.await.map_err(|e| {
                let member = self.gate().ok().and_then(|(name, home)| authorise(&conn, &members(&home).ok()?, &name).ok());
                member.and_then(|remote| self.refused(&conn, &remote)).unwrap_or(e)
            })
        };
        if let Err(e) = &result {
            conn.close(1u32.into(), e.as_bytes());
        }
        vectors::reindexed(&self.home, result).await
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }

    /// The session with `peer` after both sides know who they talk to (module doc, steps 2–6).
    async fn exchange(
        &self,
        peer: Peer<'_>,
        dialler: bool,
        tx: &mut SendStream,
        rx: &mut RecvStream,
        mine: Membership,
        ask: &[String],
    ) -> Result<Report, String> {
        let Peer { key: remote, alpn } = peer;
        let home = self.home.as_path();
        let mut report = Report::default();
        let Msg::Membership { docs } = swap(dialler, tx, rx, Msg::Membership { docs: Membership::history(home) }).await? else {
            return Err("protocol: expected membership".into());
        };
        let mine = {
            let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let (held, _) = Membership::adopt(home, &docs).unwrap_or((mine, false));
            report.quarantined += origin::apply_if_new(home, &held)?;
            steer::enforce(home, &held, borg::learn::now_ms()); // a node that lost the right to steer, or an expired steer, stops here
            held
        };
        if mine.role_of(remote).is_none() {
            return Err(format!("{remote} is no longer a member of {}", mine.collective));
        }
        self.stamp()?;
        let origins = origin::load(home);

        // Lessons, and the other synced logs: reconcile with everyone. The playbooks: followed, nothing kept.
        for file in synced(alpn) {
            if file == PLAYBOOKS {
                self.skip(dialler, tx, rx, (remote.to_string(), file.to_string(), 0)).await?;
            } else {
                self.reconcile(dialler, tx, rx, &home.join(file), &origins, &mine, alpn == V13_ALPN, &mut report).await?;
            }
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

        // Live: presence, ephemeral (live.rs).
        let now = borg::learn::now_ms();
        let live = Msg::Live { presence: live::outgoing(home, &self.me, now), directives: Directives };
        self.gate()?; // again just before anything leaves
        let Msg::Live { presence, .. } = swap(dialler, tx, rx, live).await? else {
            return Err("protocol: expected live".into());
        };
        let _w = self.store.lock().unwrap_or_else(|e| e.into_inner());
        report.presence += live::receive(home, &mine, &me, presence, now)?;
        Ok(report)
    }

    /// One log reconciled with the peer (step 3): the admitted set, recon rounds, then the lines each
    /// side lacks, signed, stored when their origin verifies and is admitted. `v1`: the peer speaks
    /// `V13_ALPN` (`offered`).
    #[allow(clippy::too_many_arguments)] // one step of a session: its stream, its log, what it checks against
    async fn reconcile(
        &self,
        dialler: bool,
        tx: &mut SendStream,
        rx: &mut RecvStream,
        path: &Path,
        origins: &HashMap<Hash, Origin>,
        mine: &Membership,
        v1: bool,
        report: &mut Report,
    ) -> Result<(), String> {
        let set = offered(path, origins, mine, borg::learn::now_ms(), v1);
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
        let give = frame(&set, origins, r.give.iter());
        report.sent += give.len();
        self.gate()?; // again just before lines leave
        let Msg::Lines { lines: got } = swap(dialler, tx, rx, Msg::Lines { lines: give }).await? else {
            return Err("protocol: expected lines".into());
        };
        self.store_lines(path, got, &r.want, mine, None, report)
    }

    /// One log a peer reconciles and this node no longer keeps (the `PLAYBOOKS` step, or a log of a
    /// 0.3.0 peer's hive goal, 3b), followed in step with it and kept nowhere. `log` names it: the peer,
    /// then the goal or the file (no goal id holds a dot), then the goal's log. The listener
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

    /// Stores what a peer gave (`Store::store_lines`).
    fn store_lines(
        &self,
        path: &Path,
        got: Vec<Signed>,
        want: &BTreeSet<Hash>,
        m: &Membership,
        author: Option<&str>,
        report: &mut Report,
    ) -> Result<(), String> {
        self.local().store_lines(path, got, want, m, author, report)
    }

    /// Writes what `stored` decided (`Store::store_messages`).
    #[cfg(test)]
    fn store_messages(
        &self,
        path: &Path,
        keep: BTreeMap<Hash, Signed>,
        decided: (&str, Stored),
        m: &Membership,
        now: i64,
        report: &mut Report,
    ) -> Result<steer::Admitted, String> {
        self.local().store_messages(path, keep, decided, m, now, report)
    }
}

/// One node's store as a session reads and writes it: where it is, the lock its writes take, and the
/// key that stamps its lines. `Node` stamps and merges through it alone; built over a store directly,
/// it does the same without an endpoint, for simulation and tests (tests/two_nodes_cli.rs), so they
/// run the merge a session runs.
pub struct Store<'a> {
    /// The collective's store.
    pub home: &'a Path,
    /// Serialises the writes to the store (`Node`'s, shared by the concurrent sessions of `serve`).
    pub lock: &'a Mutex<()>,
    /// This node's key: its own lines are stamped with it, and steering it receives admitted under it.
    pub me: &'a Identity,
}

impl Store<'_> {
    /// Stamps the lines this node's hooks wrote since the last call as its own (origin.rs).
    pub fn stamp(&self) -> Result<usize, String> {
        let _w = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Fresh outcomes first, so they are stamped in the same step.
        borg::outcome::publish(self.home);
        origin::stamp(self.home, self.me, &logs(self.home), borg::learn::now_ms())
    }

    /// Stores the lines that were asked for, whose origin verifies and is admitted (and, for
    /// replicas, is the peer itself), and that may travel under it (lines.rs `travels`): origin
    /// records first, so a line is never re-stamped as ours.
    pub fn store_lines(
        &self,
        path: &Path,
        got: Vec<Signed>,
        want: &BTreeSet<Hash>,
        m: &Membership,
        author: Option<&str>,
        report: &mut Report,
    ) -> Result<(), String> {
        let (file, now) = (log_name(path), borg::learn::now_ms());
        let mut keep: BTreeMap<Hash, Signed> = BTreeMap::new();
        for s in got {
            let h = lines::hash(&s.line);
            let ok = want.contains(&h)
                && s.origin.verify(&s.line).is_ok()
                && m.admits(&s.origin.key, s.origin.ms)
                && author.is_none_or(|a| a == s.origin.key)
                && lines::travels(file, &s.line, &s.origin.key, now);
            if ok {
                keep.insert(h, s);
            } else {
                report.rejected += 1;
            }
        }
        let _w = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if file != borg::msg::FILE {
            let known = origin::load(self.home);
            origin::append(self.home, &keep.iter().filter(|(h, _)| !known.contains_key(*h)).map(|(_, s)| s.origin.clone()).collect::<Vec<_>>())?;
            report.received += lines::merge(path, &keep.into_values().map(|s| s.line).collect::<Vec<_>>())?;
            return Ok(());
        }
        // Messages: decided over the log as read now (`stored`), before its lock, so a hook's `ack` or a
        // `send` never waits on the window's arithmetic.
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let decided = stored(&text, &keep, now);
        let a = self.store_messages(path, keep, (&text, decided), m, now, report)?;
        (report.steered, report.dropped) = (report.steered + a.new, report.dropped + a.dropped);
        drop(_w);
        // Outside both locks: it rereads the log and the outcomes, and a `send` must not wait on it.
        if a.new > 0 {
            borg::veto::apply_messages(self.home, now);
        }
        Ok(())
    }

    /// Writes what `stored` decided over the message log at `path` as it read (`decided`: that text, and
    /// what it takes of the offered `keep`), under the log's lock (borg msg.rs `send` and `ack` take it
    /// too). A log that a `send` or an `ack` changed since is decided again first, so a line written in
    /// between counts in the window and is never lost to a rewrite. A decision that writes nothing takes
    /// no lock: it holds as of its read. The write is recorded for the log's readers (msg.rs `wrote`): seen
    /// lines alone leave its talk, so they make no hook or waiter read it again. Then the privileged lines
    /// taken are admitted (steer.rs `admit`).
    fn store_messages(
        &self,
        path: &Path,
        mut keep: BTreeMap<Hash, Signed>,
        decided: (&str, Stored),
        m: &Membership,
        now: i64,
        report: &mut Report,
    ) -> Result<steer::Admitted, String> {
        let (read, first) = decided;
        if first.taken.is_empty() && first.body.is_none() {
            report.rejected += first.refused;
            return Ok(steer::Admitted::default());
        }
        let known = origin::load(self.home);
        borg::live::locked(path, || {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let d = if text == read { first } else { stored(&text, &keep, now) };
            keep.retain(|h, _| d.taken.contains(h));
            report.rejected += d.refused;
            let new: Vec<borg::msg::Message> = keep.values().filter_map(|s| serde_json::from_str(&s.line).ok()).collect();
            origin::append(self.home, &keep.iter().filter(|(h, _)| !known.contains_key(*h)).map(|(_, s)| s.origin.clone()).collect::<Vec<_>>())?;
            let io = |e: std::io::Error| format!("{}: {e}", path.display());
            let before = borg::msg::mark_of(path);
            let talk = keep.values().any(|s| !matches!(Line::parse(&s.line), Some(Line::Seen(_))));
            match &d.body {
                Some(body) => {
                    borg::steer::rewrite(path, body).map_err(io)?;
                    borg::msg::wrote(path, before.as_deref(), borg::msg::Change::rewrite(&text, body, now), talk);
                }
                None if keep.is_empty() => {}
                None => {
                    // New to the text read under the lock, so the lines are appended as they are, each on a line of its own.
                    let open = !text.is_empty() && !text.ends_with('\n');
                    let out: String = std::iter::once(if open { "\n" } else { "" }).chain(keep.values().flat_map(|s| [s.line.as_str(), "\n"])).collect();
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .and_then(|mut f| std::io::Write::write_all(&mut f, out.as_bytes()))
                        .map_err(io)?;
                    borg::msg::wrote(path, before.as_deref(), borg::msg::Change::Appended(out.len()), talk);
                }
            }
            report.received += keep.len();
            // After the write: a hook that sees the line before its admission shows it at its next call.
            steer::admit(self.home, m, &self.me.public(), &new, now).map_err(|e| format!("admissions: {e}"))
        })
    }
}

/// What the message log takes of the lines a peer offered (`stored`).
struct Stored {
    /// The offered lines taken.
    taken: BTreeSet<Hash>,
    /// How many offered lines the log lacked and refused: outside their window, or cut by the bound.
    refused: usize,
    /// The log's new body when the bound cuts a held line; otherwise the taken lines are appended.
    body: Option<String>,
}

/// What the message log holding `text` takes at `now` of `keep`, the lines a peer offered that verified
/// (`Store::store_lines`): of those it lacks, the ones within the window over held and new together
/// (`window`), and of those what the log's bound keeps (borg msg.rs `bound`). A new line the bound cuts
/// is refused, so it is not taken again when offered again; a held line it cuts goes. Pure: nodes that
/// hold the same lines and are offered the same take the same.
fn stored(text: &str, keep: &BTreeMap<Hash, Signed>, now: i64) -> Stored {
    let file: Vec<(Hash, &str)> = text.lines().map(|l| (lines::hash(l), l)).collect();
    let held: HashSet<&Hash> = file.iter().map(|(h, _)| h).collect();
    let new: Vec<(&Hash, &Signed)> = keep.iter().filter(|(h, _)| !held.contains(h)).collect();
    let lacked = new.len();
    let paced: Vec<(&Hash, &Signed)> = if new.is_empty() {
        new
    } else {
        let all: BTreeMap<Hash, String> = file.iter().map(|(h, l)| (*h, l.to_string())).chain(new.iter().map(|(h, s)| (**h, s.line.clone()))).collect();
        let w = window(&all);
        new.into_iter().filter(|(h, _)| w.contains(*h)).collect()
    };
    let (kept, _) = borg::msg::bound(file.iter().map(|(_, l)| *l).chain(paced.iter().map(|(_, s)| s.line.as_str())), now);
    let set: HashSet<&str> = kept.iter().copied().collect();
    let taken: BTreeSet<Hash> = paced.iter().filter(|(_, s)| set.contains(s.line.as_str())).map(|(h, _)| **h).collect();
    let body = file.iter().any(|(_, l)| !set.contains(l)).then(|| kept.iter().map(|l| format!("{l}\n")).collect());
    Stored { refused: lacked - taken.len(), taken, body }
}

/// The lines of the log at `path` this node offers at `now`: `travelling`, and for the messages those
/// within their `window`; to a `V13_ALPN` peer (`v1`) only what 0.5.0's window takes of them
/// (`v1_window`). Public for simulation and tests: it is the set a session reconciles (step 3).
pub fn offered(path: &Path, origins: &HashMap<Hash, Origin>, m: &Membership, now: i64, v1: bool) -> BTreeMap<Hash, String> {
    let mut set = travelling(path, origins, m, now);
    if log_name(path) == borg::msg::FILE {
        let w = window(&lines::read(path));
        set.retain(|h, _| w.contains(h));
        if v1 {
            let w = v1_window(&set, v1_share(m));
            set.retain(|h, _| w.contains(h));
        }
    }
    set
}

/// The lines of the log at `path` that are admitted and may travel under their origin at `now`
/// (lines.rs `travels`).
fn travelling(path: &Path, origins: &HashMap<Hash, Origin>, m: &Membership, now: i64) -> BTreeMap<Hash, String> {
    let file = log_name(path);
    let mut set = origin::admitted(lines::read(path), origins, m);
    set.retain(|h, l| origins.get(h).is_some_and(|o| lines::travels(file, l, &o.key, now)));
    set
}

/// The message lines of `set` within their pacing: every steer and seen line, and each tell or ask
/// within what every window admits (borg msg.rs `within_windows`: acknowledged, or at most
/// `WINDOW_CAP` unacknowledged from its sender to its recipient within `V1_RATE_MS` from it). It never
/// refuses what a sender's `send` wrote, and it reads no line older than the one it decides, so the TTL
/// and the bound never take a held line out of it: what a node holds it offers while the line travels,
/// and a node that missed a line takes it later, its acknowledged predecessors pruned or not. It depends
/// on the lines alone, so nodes holding the same lines take the same, whatever order they arrived in. A
/// line that is not well formed is outside.
fn window(set: &BTreeMap<Hash, String>) -> BTreeSet<Hash> {
    let (hashes, all): (Vec<Hash>, Vec<Line>) = set.iter().filter_map(|(h, l)| Line::parse(l).map(|x| (*h, x))).unzip();
    let admitted = borg::msg::within_windows(&all);
    hashes
        .into_iter()
        .zip(&all)
        .filter(|(_, l)| match l {
            Line::Seen(_) => true,
            Line::Message(m) => m.kind.privileged() || admitted.contains(m.id.as_str()),
        })
        .map(|(h, _)| h)
        .collect()
}

/// How many messages of one node a `V13_ALPN` peer keeps: an equal share of `V1_KEEP` per member (its
/// `per_node`).
fn v1_share(m: &Membership) -> usize {
    (V1_KEEP / m.members.len().max(1)).max(1)
}

/// Whether `from`'s rate slot to all on a `V13_ALPN` peer (`v1_window`: per node, sender and recipient,
/// the earliest within `V1_RATE_MS` wins) holds a tell or ask of `messages` from the last `V1_RATE_MS`
/// at `now`: a message written now would not be offered to that peer.
fn v1_slot_held(messages: &[borg::msg::Message], from: &borg::msg::Sender, now: i64) -> bool {
    messages
        .iter()
        .any(|m| m.v == 1 && !m.kind.privileged() && m.node == from.node && m.by == from.by && m.to == borg::msg::ALL && now.saturating_sub(m.ms) < V1_RATE_MS)
}

/// What a `V13_ALPN` peer's window (0.5.0's `window`) takes of `set`: only v1 messages (no v2 text, no
/// seen line), per node, sender and recipient one tell or ask per `V1_RATE_MS` (the earliest wins; a
/// steer is not paced), then for conversation and for steers apart the `per_node` newest per node and
/// the `V1_KEEP` newest of those. Ordered by (ms, hash), as that peer orders them.
fn v1_window(set: &BTreeMap<Hash, String>, per_node: usize) -> BTreeSet<Hash> {
    let mut all: Vec<(i64, Hash, borg::msg::Message)> = set
        .iter()
        .filter_map(|(h, l)| serde_json::from_str::<borg::msg::Message>(l).ok().filter(|m| m.v == 1 && m.well_formed()).map(|m| (m.ms, *h, m)))
        .collect();
    all.sort_by_key(|x| (x.0, x.1));
    let mut last: HashMap<(String, String, String), i64> = HashMap::new();
    let mut paced: Vec<(Hash, String, bool)> = Vec::new();
    for (ms, h, m) in all {
        if m.kind.privileged() {
            paced.push((h, m.node, true));
            continue;
        }
        let slot = last.entry((m.node.clone(), m.by, m.to)).or_insert(i64::MIN);
        if ms.saturating_sub(*slot) >= V1_RATE_MS {
            *slot = ms;
            paced.push((h, m.node, false));
        }
    }
    let mut per: HashMap<(String, bool), usize> = HashMap::new();
    let mut total = [0usize; 2];
    paced
        .into_iter()
        .rev()
        .filter(|(_, node, steers)| {
            let n = per.entry((node.clone(), *steers)).or_default();
            *n += 1;
            *n <= per_node
        })
        .filter(|(_, _, steers)| {
            let t = &mut total[usize::from(*steers)];
            *t += 1;
            *t <= V1_KEEP
        })
        .map(|(h, _, _)| h)
        .collect()
}

/// A log's file name, as lines.rs `SYNCED` lists it.
fn log_name(path: &Path) -> &str {
    path.file_name().and_then(|f| f.to_str()).unwrap_or_default()
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

/// The engineer of member `key`'s node at `now`: the newest of its drones' presence records that names
/// one (borg presence.rs `remote`), else the one its last version record kept.
fn engineer_of(home: &Path, key: &str, now: i64) -> Option<String> {
    let seen = borg::presence::remote(home, now).into_iter().filter(|r| r.key == key && !r.p.engineer.is_empty()).max_by_key(|r| r.p.ms);
    seen.map(|r| r.p.engineer).or_else(|| borg::protocol::versions(home).remove(key).and_then(|v| v.engineer))
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

/// The `Lines` frame a session gives of a log after its reconciliation (step 3): the lines of `set`
/// (`offered`) named by `hashes`, the ones the peer lacks (`Recon::give`), each with its signed origin,
/// as many as the frame's budget takes (`within_budget`). Public for simulation and tests, which hand
/// it to the peer instead of a stream.
pub fn frame<'a>(set: &BTreeMap<Hash, String>, origins: &HashMap<Hash, Origin>, hashes: impl Iterator<Item = &'a Hash>) -> Vec<Signed> {
    within_budget(signed(set, origins, hashes))
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

    /// A store upgraded from 0.4.0 still holds playbooks.jsonl, stamped: it is neither offered nor
    /// taken, and the session still steps through it, so the two nodes stay in order.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_leftover_playbooks_log_is_not_synced() {
        let (pa, pb) = team("playbooks");
        let (a, b) = (home(&pa), home(&pb));
        let edit = json!({"pb": "p1", "edit": "e1", "step": "s1", "text": "run the parser tests", "ms": 1}).to_string();
        write(&a, PLAYBOOKS, std::slice::from_ref(&edit));
        let h = lines::hash(&edit);
        origin::append(&a, &[Origin::new(&Identity::load_or_create(&a).unwrap(), &h, 1)]).unwrap();
        let c = json!({"id": "cabc", "text": "locks are taken in id order", "lesson": "L1", "by": "lead@x", "ms": 5, "source": "veto"}).to_string();
        write(&a, borg::veto::FILE, std::slice::from_ref(&c));
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let (served, synced) = (served.unwrap(), synced.unwrap());
        assert_eq!((synced.received, served.sent), (1, 1), "only the constraint, the step after the playbooks', moves");
        assert!(holds(&b, borg::veto::FILE, &c));
        assert!(!b.join(PLAYBOOKS).exists(), "no playbook reaches bob");
        assert!(holds(&a, PLAYBOOKS, &edit), "alice's leftover stays untouched");
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

    /// An absorb or release of lesson `id`, as borg `memory absorb` and `release` write it.
    fn absorb(id: &str, kind: &str, ms: i64, note: Option<&str>) -> String {
        let mut v = json!({"id": id, "kind": kind, "ms": ms, "by": "lead@x"});
        if let Some(n) = note {
            (v["note"], v["version"]) = (json!(n), json!(1));
        }
        v.to_string()
    }

    /// Absorbs and releases reach the other member signed, both ways; a line the write path would have
    /// refused (an oversize note) is never offered, and a resync is quiet over every synced log.
    #[tokio::test(flavor = "multi_thread")]
    async fn absorbs_reach_the_team_signed_and_a_malformed_one_stays_home() {
        let (pa, pb) = team("absorbs");
        let (a, b) = (home(&pa), home(&pb));
        let (held, released) = (absorb("L1", "absorb", 5, Some("skills/review@2026-10-30")), absorb("L2", "release", 6, None));
        let oversize = absorb("L3", "absorb", 7, Some(&"n".repeat(borg::absorb::NOTE_CHARS + 1)));
        write(&a, lines::ABSORBED, &[held.clone(), released.clone(), oversize.clone()]);
        let bobs = absorb("L4", "absorb", 8, Some("a team's skills repo"));
        write(&b, lines::ABSORBED, std::slice::from_ref(&bobs));
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let (served, synced) = (served.unwrap(), synced.unwrap());
        assert_eq!((synced.received, synced.rejected, served.received), (2, 0, 1), "the two well-formed lines, the oversize one never offered");
        assert!(holds(&b, lines::ABSORBED, &held) && holds(&b, lines::ABSORBED, &released) && !holds(&b, lines::ABSORBED, &oversize));
        assert!(holds(&a, lines::ABSORBED, &bobs), "bob's absorb reaches alice");
        assert!(origin::load(&b).contains_key(&lines::hash(&held)), "it arrived with its signed origin");
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        let quiet = Report { rounds: SYNCED.len(), fps: SYNCED.len(), ..Report::default() };
        assert_eq!((synced.unwrap(), served.unwrap()), (quiet, quiet), "resync is quiet");
        alice.close().await;
        bob.close().await;
    }

    /// Lines a peer gives bypass the write path's checks, so they are checked on receipt: an oversize
    /// note, an unknown kind or a line without a field the write path sets is refused, signed or not.
    #[test]
    fn a_malformed_or_oversize_absorb_from_a_peer_is_refused() {
        let (pa, pb) = team("absorb-refused");
        let (a, b) = (home(&pa), home(&pb));
        let (alice, bob) = (Identity::load_or_create(&a).unwrap(), Identity::load_or_create(&b).unwrap());
        let ok = absorb("L1", "absorb", 5, Some("skills/review@2026-10-30"));
        let bad = [
            absorb("L1", "absorb", 5, Some(&"n".repeat(borg::absorb::NOTE_CHARS + 1))),
            absorb("L1", "forget", 5, None),
            json!({"id": "L1", "kind": "release", "ms": 5}).to_string(),
        ];
        let now = borg::learn::now_ms();
        let got: Vec<Signed> =
            std::iter::once(&ok).chain(&bad).map(|l| Signed { origin: Origin::new(&alice, &lines::hash(l), now), line: l.clone() }).collect();
        let want: BTreeSet<Hash> = got.iter().map(|s| lines::hash(&s.line)).collect();
        let lock = Mutex::new(());
        let store = Store { home: &b, lock: &lock, me: &bob };
        let mut report = Report::default();
        store.store_lines(&b.join(lines::ABSORBED), got, &want, &members(&b).unwrap(), None, &mut report).unwrap();
        assert_eq!((report.received, report.rejected), (1, bad.len()));
        assert_eq!(lines::read(&b.join(lines::ABSORBED)).into_values().collect::<Vec<_>>(), [ok]);
    }

    /// A 0.6 member (`borg/mesh/14`) keeps syncing everything it syncs today, whichever side dials, and
    /// no absorb moves either way: its session has no absorbs step.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_mesh_14_peer_syncs_everything_but_the_absorbs_both_ways() {
        let p = crew("v14", &["alice", "old1", "old2"]);
        let (a, o1, o2) = (home(&p[0]), home(&p[1]), home(&p[2]));
        let keys: Vec<String> = [&a, &o1, &o2].iter().map(|h| Identity::load_or_create(h).unwrap().public()).collect();
        let now = borg::learn::now_ms();
        write(&a, "lessons.jsonl", &[lesson("L1", 1, "lock order?")]);
        let o = json!({"kind": "asked_then_changed", "session": "s1", "lessons": ["L1"], "ms": 1, "repo": "app"}).to_string();
        write(&a, "outcomes.jsonl", &[o]);
        assert_eq!(borg::outcome::publish(&a), 1);
        let tell = message(&a, &keys[0], ALL_TO, now - 100, "migrations are mine today");
        let ours = absorb("L1", "absorb", 5, Some("skills/review@2026-10-30"));
        write(&a, lines::ABSORBED, std::slice::from_ref(&ours));
        let mut theirs = Vec::new();
        for (i, h) in [(1, &o1), (2, &o2)] {
            write(h, "lessons.jsonl", &[lesson(&format!("L{}", i + 1), 1, "retry budget?")]);
            write(h, lines::ABSORBED, &[absorb(&format!("L{}", i + 1), "absorb", 6, Some("a team's skills repo"))]);
            theirs.push(message(h, &keys[i], ALL_TO, now - 50, "from a 0.6 node"));
        }
        let mut alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        alice.speaks.tell = false; // the counts below are the logs' alone; the announcement has its own tests
        let old = |i: usize| Node::open(&p[i], Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![V14_ALPN.to_vec()]));
        let (old1, old2) = (old(1).await.unwrap(), old(2).await.unwrap());
        // The older node dials.
        let ticket = alice.ticket();
        let (served, dialled) = tokio::join!(alice.accept(), old1.dial(&ticket, &[], V14_ALPN, Vec::new()));
        served.unwrap();
        dialled.unwrap();
        // This node dials the older one, offering its own protocol first.
        let old_ticket = old2.ticket();
        let (served, dialled) = tokio::join!(old2.accept(), alice.sync(&old_ticket));
        served.unwrap();
        dialled.unwrap();
        let team = borg::outcome::TEAM_FILE;
        for o in [&o1, &o2] {
            assert!(holds(o, "lessons.jsonl", &lesson("L1", 1, "lock order?")), "lessons travel");
            assert_eq!(lines::read(&o.join(team)), lines::read(&a.join(team)), "outcomes travel");
            assert!(holds(o, borg::msg::FILE, &tell), "messages travel");
            assert!(!holds(o, lines::ABSORBED, &ours), "no absorb reaches a 0.6 node");
        }
        assert!(theirs.iter().all(|l| holds(&a, borg::msg::FILE, l)), "their messages reach this node");
        assert!(holds(&a, "lessons.jsonl", &lesson("L2", 1, "retry budget?")) && holds(&a, "lessons.jsonl", &lesson("L3", 1, "retry budget?")));
        assert_eq!(lines::read(&a.join(lines::ABSORBED)).into_values().collect::<Vec<_>>(), [ours], "and none of theirs reaches it");
        assert_eq!(borg::presence::reads(&a).get(&keys[1]).copied(), Some(2), "a 0.6 node reads every message line");
        // In step: old1 takes what old2 gave alice since (its lesson and message), and then a resync
        // reconciles the five logs a 0.6 node keeps, once each, and moves nothing.
        let (served, dialled) = tokio::join!(alice.accept(), old1.dial(&ticket, &[], V14_ALPN, Vec::new()));
        assert_eq!((dialled.unwrap().received, served.unwrap().sent), (2, 2));
        assert!(!holds(&o1, lines::ABSORBED, &absorb("L3", "absorb", 6, Some("a team's skills repo"))));
        let (served, dialled) = tokio::join!(alice.accept(), old1.dial(&ticket, &[], V14_ALPN, Vec::new()));
        let quiet = Report { rounds: SYNCED.len() - 1, fps: SYNCED.len() - 1, ..Report::default() };
        assert_eq!((dialled.unwrap(), served.unwrap()), (quiet, quiet));
        for n in [alice, old1, old2] {
            n.close().await;
        }
    }

    const ALL_TO: &str = borg::msg::ALL;

    /// A message from session `s1` of the node whose member key is `node`, appended to `home`'s log.
    fn message(home: &Path, node: &str, to: &str, ms: i64, text: &str) -> String {
        let from = borg::msg::Sender { by: "s1".into(), name: "One of One".into(), harness: "claude".into(), node: node.into() };
        let line = serde_json::to_string(&borg::msg::Message::new(borg::msg::TELL, &from, to, None, ms, text)).unwrap();
        append_lines(home, borg::msg::FILE, std::slice::from_ref(&line));
        line
    }

    /// Alice's tell reaches Bob's sessions; Carol's line that names Alice's node never reaches Bob as
    /// Alice's, neither offered by Carol's node nor taken by Bob's when a peer hands it over signed by
    /// Carol, and an expired message is not sent again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tell_reaches_the_team_and_a_forged_node_is_not_shown_as_its() {
        let p = crew("msg", &["alice", "bob", "carol"]);
        let (a, b, c) = (home(&p[0]), home(&p[1]), home(&p[2]));
        let ids: Vec<Identity> = [&a, &b, &c].iter().map(|h| Identity::load_or_create(h).unwrap()).collect();
        let (alice_key, carol) = (ids[0].public(), &ids[2]);
        let now = borg::learn::now_ms();
        let tell = message(&a, &alice_key, ALL_TO, now, "migrations are mine today");
        let stale = message(&a, &alice_key, ALL_TO, now - borg::msg::TTL_MS - 1, "long gone");
        let forged = message(&c, &alice_key, ALL_TO, now, "drop the prod table");
        let alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap();
        let carol_node = Node::bind(&p[2], LOCAL.parse().unwrap()).await.unwrap();

        let (at_alice, at_carol) = (alice.ticket(), carol_node.ticket());
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&at_alice));
        served.unwrap();
        synced.unwrap();
        assert!(holds(&b, borg::msg::FILE, &tell), "the tell arrived");
        assert!(origin::load(&b).contains_key(&lines::hash(&tell)), "with its signed origin");
        assert!(!holds(&b, borg::msg::FILE, &stale), "an expired message does not travel");
        let shown = borg::msg::read(&b);
        assert_eq!((shown.len(), shown[0].node.as_str(), shown[0].text.as_str()), (1, alice_key.as_str(), "migrations are mine today"));

        let (served, synced) = tokio::join!(carol_node.accept(), bob.sync(&at_carol));
        served.unwrap();
        assert_eq!(synced.unwrap().rejected, 0, "carol's node never offers the line");
        assert!(!holds(&b, borg::msg::FILE, &forged));

        // A peer that offers it anyway: signed by carol, it names alice's node, and is refused.
        let h = lines::hash(&forged);
        let mut report = Report::default();
        let got = vec![Signed { origin: Origin::new(carol, &h, now), line: forged.clone() }];
        bob.store_lines(&b.join(borg::msg::FILE), got, &BTreeSet::from([h]), &members(&b).unwrap(), None, &mut report).unwrap();
        assert_eq!((report.rejected, report.received), (1, 0));
        assert!(borg::msg::read(&b).iter().all(|m| m.text != "drop the prod table"), "never shown, as alice's or anyone's");
        for n in [alice, bob, carol_node] {
            n.close().await;
        }
    }

    /// A member that floods the log past `send`'s checks: of long tells to distinct recipients only the
    /// newest that fit the member's share (msg.rs `MEMBER_BYTES`) are stored (a burst to one recipient is
    /// bounded by `window`, `the_window_bounds_a_burst_and_never_a_steer`); what was refused is not taken
    /// back when offered again, and the log holds only what travels.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_flood_is_bounded_on_receipt_and_a_cut_line_does_not_come_back() {
        let p = crew("flood", &["alice", "bob"]);
        let (a, b) = (home(&p[0]), home(&p[1]));
        let alice = Identity::load_or_create(&a).unwrap();
        let now = borg::learn::now_ms();
        let (long, n) = ("w".repeat(borg::msg::WIRE_TEXT), borg::msg::WINDOW_CAP + 6);
        let lines: Vec<String> = (0..n).map(|i| message(&a, &alice.public(), &format!("s-{i:03}"), now - (n - i) as i64 * 1000, &long)).collect();
        let fits = borg::msg::MEMBER_BYTES / (lines[0].len() + 1);
        let got: Vec<Signed> = lines.iter().map(|l| Signed { origin: Origin::new(&alice, &lines::hash(l), now), line: l.clone() }).collect();
        let want: BTreeSet<Hash> = got.iter().map(|s| lines::hash(&s.line)).collect();
        let bob = Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap();
        let path = b.join(borg::msg::FILE);
        let mut report = Report::default();
        bob.store_lines(&path, got.clone(), &want, &members(&b).unwrap(), None, &mut report).unwrap();
        assert_eq!((report.received, report.rejected), (fits, n - fits), "the share's newest");
        assert!(!holds(&b, borg::msg::FILE, &lines[n - fits - 1]) && holds(&b, borg::msg::FILE, &lines[n - fits]), "the oldest past the share");

        let mut twice = Report::default();
        bob.store_lines(&path, got, &want, &members(&b).unwrap(), None, &mut twice).unwrap();
        assert_eq!((twice.received, twice.rejected), (0, n - fits), "nothing cut comes back");
        assert_eq!(lines::read(&path).len(), fits);
        assert_eq!(offered(&path, &origin::load(&b), &members(&b).unwrap(), now, false).len(), fits);
        bob.close().await;
    }

    /// A node's lines for its own sessions (`local`, which never travel) have their own share of its
    /// log (borg msg.rs `bound`): however many it holds, it keeps the travelling lines its peer keeps,
    /// so no session offers back a line it cut and has it refused again, session after session.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_nodes_own_lines_never_cut_what_its_peers_keep() {
        let (pa, pb) = team("local-share");
        let (a, b) = (home(&pa), home(&pb));
        let alice = Identity::load_or_create(&a).unwrap().public();
        let bob = Identity::load_or_create(&b).unwrap().public();
        let now = borg::learn::now_ms();
        let long = "w".repeat(borg::msg::WIRE_TEXT);
        let n = borg::msg::WINDOW_CAP / 2 + borg::msg::WINDOW_CAP / 8;
        let lines_to = |to: &str, at: i64| -> Vec<String> { (0..n).map(|i| line_of(&alice, &format!("s-{i:03}"), to, at + i as i64, &long).1).collect() };
        let out = lines_to(&format!("r@{bob}"), now - 20_000);
        let here = lines_to(&format!("local@{alice}"), now - 10_000);
        let size = out[0].len().max(here[0].len()) + 1;
        assert!(n * size <= borg::msg::MEMBER_BYTES && 2 * n * size > borg::msg::MEMBER_BYTES, "each kind alone fits a share, both do not");
        append_lines(&a, borg::msg::FILE, &out);
        let (alice_node, bob_node) = (Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap(), Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap());
        let ticket = alice_node.ticket();
        let (served, synced) = tokio::join!(alice_node.accept(), bob_node.sync(&ticket));
        assert_eq!((served.unwrap().rejected, synced.unwrap().rejected), (0, 0));
        assert!(out.iter().all(|l| holds(&b, borg::msg::FILE, l)), "bob keeps every tell");
        // Alice's sessions talk among themselves; an acknowledgement bounds her log as `send` does.
        append_lines(&a, borg::msg::FILE, &here);
        let first: borg::msg::Message = serde_json::from_str(&here[0]).unwrap();
        borg::msg::ack(&a, "s-001", &[&first.id], now).unwrap();
        assert!(out.iter().chain(&here).all(|l| holds(&a, borg::msg::FILE, l)), "alice keeps what travels and what stays");
        for _ in 0..2 {
            let (served, synced) = tokio::join!(alice_node.accept(), bob_node.sync(&ticket));
            assert_eq!((served.unwrap().rejected, synced.unwrap().rejected), (0, 0), "nothing offered back, nothing refused");
        }
        alice_node.close().await;
        bob_node.close().await;
    }

    /// A send or an acknowledgement that writes the log after a session decided what it takes and
    /// before it holds the lock: the decision is made again over the log as it is then, so that line
    /// is kept by the rewrite (here the bound prunes an expired line) and counts in the windows.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_line_written_between_the_decision_and_the_lock_is_kept() {
        let (pa, pb) = team("race");
        let (a, b) = (home(&pa), home(&pb));
        let alice = Identity::load_or_create(&a).unwrap();
        let bob = Identity::load_or_create(&b).unwrap().public();
        let now = borg::learn::now_ms();
        let expired = line_of(&alice.public(), "s1", &format!("r0@{bob}"), now - borg::msg::TTL_MS - borg::steer::SLACK_MS - 1, "gone").1;
        // As many unread tells to r1 as any window lets through, and an earlier one a peer offers.
        let burst: Vec<(borg::msg::Message, String)> =
            (0..borg::msg::WINDOW_CAP).map(|i| line_of(&alice.public(), "s1", &format!("r1@{bob}"), now - 2000 + i as i64, &format!("t{i}"))).collect();
        let next = line_of(&alice.public(), "s1", &format!("r1@{bob}"), now - 3000, "earlier").1;
        let other = line_of(&alice.public(), "s2", &format!("r2@{bob}"), now - 1000, "other").1;
        append_lines(&b, borg::msg::FILE, &std::iter::once(expired.clone()).chain(burst.iter().map(|(_, l)| l.clone())).collect::<Vec<_>>());
        let path = b.join(borg::msg::FILE);
        let keep: BTreeMap<Hash, Signed> =
            [&next, &other].into_iter().map(|l| (lines::hash(l), Signed { origin: Origin::new(&alice, &lines::hash(l), now), line: l.clone() })).collect();
        let read = std::fs::read_to_string(&path).unwrap();
        let decided = stored(&read, &keep, now);
        assert_eq!((decided.taken.len(), decided.refused, decided.body.is_some()), (1, 1, true), "one more unread tell to r1 than any window lets through");
        // Bob's r1 is shown one of them in between.
        let seen = seen_of(&bob, "r1", now - 500, &[&burst[0].0.id]);
        append_lines(&b, borg::msg::FILE, std::slice::from_ref(&seen));
        let bob_node = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let mut report = Report::default();
        bob_node.store_messages(&path, keep, (&read, decided), &members(&b).unwrap(), now, &mut report).unwrap();
        assert!(holds(&b, borg::msg::FILE, &seen), "the acknowledgement written in between stays");
        assert!(holds(&b, borg::msg::FILE, &next) && holds(&b, borg::msg::FILE, &other), "and it let the earlier tell in");
        assert!(!holds(&b, borg::msg::FILE, &expired), "the bound still pruned");
        assert_eq!((report.received, report.rejected), (2, 0));
        bob_node.close().await;
    }

    /// A message of `node`'s session `by` to `to` at `ms`, as a line.
    fn line_of(node: &str, by: &str, to: &str, ms: i64, text: &str) -> (borg::msg::Message, String) {
        let from = borg::msg::Sender { by: by.into(), name: "One of One".into(), harness: "claude".into(), node: node.into() };
        let m = borg::msg::Message::new(borg::msg::TELL, &from, to, None, ms, text);
        let line = serde_json::to_string(&m).unwrap();
        (m, line)
    }

    /// A seen line of `node`'s session `by` for `ids`.
    fn seen_of(node: &str, by: &str, ms: i64, ids: &[&str]) -> String {
        serde_json::to_string(&borg::msg::Seen::new(by, node, ms, ids)).unwrap()
    }

    /// A recipient's seen line travels to the sender's node and opens its window there; a seen line
    /// another node writes for that session does not.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_seen_line_from_the_recipients_node_opens_the_senders_window() {
        let p = crew("seen", &["alice", "bob", "carol"]);
        let (a, b, c) = (home(&p[0]), home(&p[1]), home(&p[2]));
        let keys: Vec<String> = [&a, &b, &c].iter().map(|h| Identity::load_or_create(h).unwrap().public()).collect();
        let now = borg::learn::now_ms();
        let to = format!("s2@{}", keys[1]);
        let from = borg::msg::Sender { by: "s1".into(), name: "One of One".into(), harness: "claude".into(), node: keys[0].clone() };
        let tell = |ms: i64, text: &str| borg::msg::send(&a, borg::msg::Message::new(borg::msg::TELL, &from, &to, None, ms, text), ms);
        let x = tell(now, "one").unwrap();
        assert!(matches!(tell(now + 1, "two"), Err(borg::msg::SendError::Window { outstanding: 1, .. })));
        let (alice, bob, carol) = (
            Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap(),
            Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap(),
            Node::bind(&p[2], LOCAL.parse().unwrap()).await.unwrap(),
        );
        let ticket = alice.ticket();
        // Carol's node claims s2 saw it: its own line, which travels, and acknowledges nothing.
        let forged = seen_of(&keys[2], "s2", now, &[&x.id]);
        append_lines(&c, borg::msg::FILE, std::slice::from_ref(&forged));
        let (served, synced) = tokio::join!(alice.accept(), carol.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        assert!(holds(&a, borg::msg::FILE, &forged), "a seen line travels under its node");
        assert!(matches!(tell(now + 2, "two"), Err(borg::msg::SendError::Window { .. })), "but s2 is bob's");
        // Bob's s2 is shown the tell: its seen line reaches alice, and the window opens.
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        let caps = borg::harness::by_name("claude").unwrap().caps();
        let out = borg::msg::deliver(&b, caps, "prompt", &json!({"session_id": "s2"}), Some(now - 10), now + 3, None).expect("shown");
        assert_eq!(out["shown_ids"], json!([x.id]));
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        assert_eq!(synced.unwrap().rejected, 0);
        tell(now + 4, "two").expect("acknowledged on bob's node");
        assert_eq!(borg::msg::read(&b), vec![x], "no seen line is read as a message");
        for n in [alice, bob, carol] {
            n.close().await;
        }
    }

    /// A tell admitted when written still travels once the acknowledged message that opened its window is
    /// pruned: alice's s1 had its first tell to s2 acknowledged, so its window let two more through over an
    /// hour later; past the first one's TTL alice's log holds only those two, over which `window_for`
    /// would admit one, and bob, who missed them, takes both from alice, and carol both from bob.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tell_admitted_when_written_travels_after_what_opened_its_window_is_pruned() {
        use borg::msg::{Author, Message, SendError, Sender, TELL, TTL_MS};
        let p = crew("pruned", &["alice", "bob", "carol"]);
        let (a, b, c) = (home(&p[0]), home(&p[1]), home(&p[2]));
        let key = Identity::load_or_create(&a).unwrap().public();
        let now = borg::learn::now_ms();
        let s1 = Sender { by: "s1".into(), name: "One of One".into(), harness: "claude".into(), node: key.clone() };
        let to = format!("s2@{key}");
        let tell = |ms: i64, text: &str| borg::msg::send(&a, Message::new(TELL, &s1, &to, None, ms, text), ms);
        // Past its TTL and the bound's slack by now; the two after it an hour and more later, still fresh.
        let t = now - TTL_MS - borg::steer::SLACK_MS - 60_000;
        let first = tell(t, "first").unwrap();
        borg::msg::ack(&a, "s2", &[&first.id], t + 1).unwrap();
        let t2 = t + 2 * borg::steer::SLACK_MS;
        let (second, third) = (tell(t2, "second").unwrap(), tell(t2 + 1, "third").unwrap());
        assert!(matches!(tell(t2 + 2, "fourth"), Err(SendError::Window { outstanding: 2, window: 2, .. })), "the acknowledgement doubled it");
        // Any write now prunes what is past its TTL: here another session's acknowledgement.
        borg::msg::ack(&a, "s7", &[&second.id], now).unwrap();
        let held = borg::msg::lines(&a);
        assert!(held.iter().all(|l| l.message().is_none_or(|m| m.id != first.id)), "the first is pruned");
        let w = borg::msg::window_for(&held, Author { node: &key, by: "s1" }, &to, now);
        assert!(w.admits(&second.id) && !w.admits(&third.id), "a replay over what is left admits less than send did");
        let (alice, bob, carol) = (
            Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap(),
            Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap(),
            Node::bind(&p[2], LOCAL.parse().unwrap()).await.unwrap(),
        );
        let line = |m: &Message| serde_json::to_string(m).unwrap();
        let (from_alice, from_bob) = (alice.ticket(), bob.ticket());
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&from_alice));
        served.unwrap();
        assert_eq!(synced.unwrap().rejected, 0);
        assert!(holds(&b, borg::msg::FILE, &line(&second)) && holds(&b, borg::msg::FILE, &line(&third)), "bob takes both from alice");
        let (served, synced) = tokio::join!(bob.accept(), carol.sync(&from_bob));
        served.unwrap();
        assert_eq!(synced.unwrap().rejected, 0);
        assert!(holds(&c, borg::msg::FILE, &line(&second)) && holds(&c, borg::msg::FILE, &line(&third)), "and carol both from bob");
        for n in [alice, bob, carol] {
            n.close().await;
        }
    }

    /// A seen line merged from a peer moves the message log but not its talk (borg msg.rs `talk_mark`),
    /// so no hook or waiter on this node reads the log again for it; a tell merged moves it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_merged_seen_line_leaves_the_logs_talk_and_a_merged_tell_moves_it() {
        let p = crew("talk", &["alice", "bob"]);
        let (a, b) = (home(&p[0]), home(&p[1]));
        let keys: Vec<String> = [&a, &b].iter().map(|h| Identity::load_or_create(h).unwrap().public()).collect();
        let now = borg::learn::now_ms();
        let sender = |node: &str, by: &str| borg::msg::Sender { by: by.into(), name: "One of One".into(), harness: "claude".into(), node: node.into() };
        let x = borg::msg::Message::new(borg::msg::TELL, &sender(&keys[0], "s1"), &format!("s2@{}", keys[1]), None, now, "one");
        let x = borg::msg::send(&a, x, now).unwrap();
        let (alice, bob) = (Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap(), Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap());
        let ticket = alice.ticket();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        let caps = borg::harness::by_name("claude").unwrap().caps();
        let out = borg::msg::deliver(&b, caps, "prompt", &json!({"session_id": "s2"}), Some(now - 10), now + 1, None).expect("shown on bob's node");
        assert_eq!(out["shown_ids"], json!([x.id]));
        let (log, talk) = (borg::msg::mark_of(&borg::msg::path(&a)), borg::msg::talk_mark(&a));
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        assert_eq!(synced.unwrap().rejected, 0);
        assert!(
            lines::read(&borg::msg::path(&a)).values().any(|l| matches!(Line::parse(l), Some(Line::Seen(s)) if s.ids == [x.id.clone()])),
            "the seen line merged"
        );
        assert_ne!(borg::msg::mark_of(&borg::msg::path(&a)), log, "the log moved");
        assert_eq!(borg::msg::talk_mark(&a), talk, "its talk did not");
        // Bob's s2 answers: a tell merged moves the talk, and alice's s1 is shown it.
        let reply = borg::msg::Message::new(borg::msg::TELL, &sender(&keys[1], "s2"), &format!("s1@{}", keys[0]), Some(&x.id), now + 2, "done");
        let reply = borg::msg::send(&b, reply, now + 2).unwrap();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        synced.unwrap();
        assert_ne!(borg::msg::talk_mark(&a), talk, "a tell moves it");
        let out = borg::msg::deliver(&a, caps, "prompt", &json!({"session_id": "s1"}), Some(now - 10), now + 3, None).expect("shown on alice's node");
        assert_eq!(out["shown_ids"], json!([reply.id]));
        for n in [alice, bob] {
            n.close().await;
        }
    }

    /// A 0.4.0 or 0.5.x member (`borg/mesh/13`) is offered only what it reads and keeps, whichever side
    /// dials: v1 lines in 0.5.0's pacing, never a v2 text or a seen line. Its own lines reach this node.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_mesh_13_peer_gets_only_v1_lines_in_v1_pacing_both_ways() {
        let p = crew("v13", &["alice", "old1", "old2"]);
        let (a, o1, o2) = (home(&p[0]), home(&p[1]), home(&p[2]));
        let keys: Vec<String> = [&a, &o1, &o2].iter().map(|h| Identity::load_or_create(h).unwrap().public()).collect();
        let now = borg::learn::now_ms();
        let to = format!("s-r@{}", keys[0]);
        let (first, first_line) = line_of(&keys[0], "s1", &to, now - 3000, "first");
        let seen = seen_of(&keys[0], "s-r", now - 2500, &[&first.id]);
        let (second, second_line) = line_of(&keys[0], "s1", &to, now - 2000, "second");
        let (long, long_line) = line_of(&keys[0], "s1", &format!("s-x@{}", keys[0]), now - 1000, &"w".repeat(borg::msg::V1_TEXT + 1));
        assert_eq!((first.v, second.v, long.v), (1, 1, 2));
        append_lines(&a, borg::msg::FILE, &[first_line.clone(), seen.clone(), second_line.clone(), long_line.clone()]);
        let theirs: Vec<String> = [(&o1, &keys[1]), (&o2, &keys[2])].iter().map(|(h, k)| message(h, k, ALL_TO, now - 100, "from an older node")).collect();
        let alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        let old = |i: usize| Node::open(&p[i], Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![V13_ALPN.to_vec()]));
        let (old1, old2) = (old(1).await.unwrap(), old(2).await.unwrap());
        // The older node dials.
        let ticket = alice.ticket();
        let (served, dialled) = tokio::join!(alice.accept(), old1.dial(&ticket, &[], V13_ALPN, Vec::new()));
        served.unwrap();
        dialled.unwrap();
        // This node dials the older one, offering its own protocol first.
        let old_ticket = old2.ticket();
        let (served, dialled) = tokio::join!(old2.accept(), alice.sync(&old_ticket));
        served.unwrap();
        dialled.unwrap();
        for o in [&o1, &o2] {
            assert!(holds(o, borg::msg::FILE, &first_line), "a v1 line travels");
            assert!(!holds(o, borg::msg::FILE, &second_line), "0.5.0's pacing: one per sender and recipient per rate slot");
            assert!(!holds(o, borg::msg::FILE, &long_line) && !holds(o, borg::msg::FILE, &seen), "no v2 text, no seen line");
            let v1 = |l: &String| serde_json::from_str::<borg::msg::Message>(l).is_ok_and(|m| m.v == 1);
            assert!(lines::read(&o.join(borg::msg::FILE)).values().all(v1), "every line a 0.5.0 reader reads");
        }
        assert!(theirs.iter().all(|l| holds(&a, borg::msg::FILE, l)), "their lines reach this node");
        for n in [alice, old1, old2] {
            n.close().await;
        }
    }

    /// A session on a 0.4.0 or 0.5.x node (`borg/mesh/13`) acknowledges nothing, so this node's `send`
    /// paces its sender as 0.5.0 did (borg msg.rs `Pace::Rate`): two tells a rate slot apart both go,
    /// unread, and that node takes both.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_tells_a_rate_slot_apart_reach_a_mesh_13_node() {
        let p = crew("v13-rate", &["alice", "old"]);
        let (a, o) = (home(&p[0]), home(&p[1]));
        let keys: Vec<String> = [&a, &o].iter().map(|h| Identity::load_or_create(h).unwrap().public()).collect();
        let now = borg::learn::now_ms();
        let from = borg::msg::Sender { by: "s1".into(), name: "One of One".into(), harness: "claude".into(), node: keys[0].clone() };
        let to = format!("s9@{}", keys[1]);
        let tell = |ms: i64, text: &str| borg::msg::send(&a, borg::msg::Message::new(borg::msg::TELL, &from, &to, None, ms, text), ms);
        let first = tell(now - V1_RATE_MS - 1000, "first").unwrap();
        assert!(matches!(tell(now - V1_RATE_MS, "too soon"), Err(borg::msg::SendError::Window { pace: borg::msg::Pace::Rate, .. })));
        let second = tell(now - 500, "a slot later").unwrap();
        let alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        let old = Node::open(&p[1], Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![V13_ALPN.to_vec()])).await.unwrap();
        let ticket = alice.ticket();
        let (served, dialled) = tokio::join!(alice.accept(), old.dial(&ticket, &[], V13_ALPN, Vec::new()));
        served.unwrap();
        dialled.unwrap();
        let line = |m: &borg::msg::Message| serde_json::to_string(m).unwrap();
        assert!(holds(&o, borg::msg::FILE, &line(&first)) && holds(&o, borg::msg::FILE, &line(&second)), "both offered and taken");
        for n in [alice, old] {
            n.close().await;
        }
    }

    /// A text longer than a 0.4.0 or 0.5.x node (`borg/mesh/13`) reads is refused for its sessions at
    /// `send`, saying why: that node would never show it, and it would hold the sender's window for a
    /// week. A short text goes, so does a long one for the collective, and once the node's sessions run
    /// on this protocol, a long one for it too.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_text_a_mesh_13_node_cannot_read_is_refused_for_its_sessions() {
        let p = crew("v13-long", &["alice", "old"]);
        let (a, o) = (home(&p[0]), home(&p[1]));
        let keys: Vec<String> = [&a, &o].iter().map(|h| Identity::load_or_create(h).unwrap().public()).collect();
        let now = borg::learn::now_ms();
        let from = borg::msg::Sender { by: "s1".into(), name: "One of One".into(), harness: "claude".into(), node: keys[0].clone() };
        let tell = |to: &str, ms: i64, text: &str| borg::msg::send(&a, borg::msg::Message::new(borg::msg::TELL, &from, to, None, ms, text), ms);
        let long = "w".repeat(borg::msg::V1_TEXT + 1);
        let alice = Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap();
        let old = Node::open(&p[1], Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![V13_ALPN.to_vec()])).await.unwrap();
        let ticket = alice.ticket();
        let (served, dialled) = tokio::join!(alice.accept(), old.dial(&ticket, &[], V13_ALPN, Vec::new()));
        served.unwrap();
        dialled.unwrap();
        assert_eq!(borg::presence::reads(&a).get(&keys[1]), Some(&1));
        let to = format!("s9@{}", keys[1]);
        let refused = tell(&to, now, &long).unwrap_err();
        assert!(matches!(&refused, borg::msg::SendError::Reads { v: 1, .. }), "{refused:?}");
        assert!(refused.to_string().contains(&format!("at most {} characters", borg::msg::V1_TEXT)), "{refused}");
        tell(&to, now + 1, "short").expect("a v1 text goes, and nothing held the window");
        tell(ALL_TO, now + 2, &long).expect("the other members read it");
        let (served, dialled) = tokio::join!(alice.accept(), old.dial(&ticket, &[], ALPN, Vec::new()));
        served.unwrap();
        dialled.unwrap();
        tell(&format!("s8@{}", keys[1]), now + 3, &long).expect("upgraded");
        alice.close().await;
        old.close().await;
    }

    /// Two members that see a membership change at different times keep the same messages: neither the
    /// window nor the bound depends on how many members there are (0.5.0 shared its line bound among
    /// them, so the node that saw a third member first cut lines the other still offered).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_membership_change_seen_at_different_times_keeps_the_messages() {
        let (pa, pb) = team("members");
        let (a, b) = (home(&pa), home(&pb));
        let alice_id = Identity::load_or_create(&a).unwrap();
        let bob_key = Identity::load_or_create(&b).unwrap().public();
        let now = borg::learn::now_ms();
        let n = V1_KEEP / 2 + 50;
        let sent: Vec<String> = (0..n).map(|i| message(&a, &alice_id.public(), &format!("s-{i:03}@{bob_key}"), now - (n - i) as i64, "hi")).collect();
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let bob = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let (ticket, bob_ticket) = (alice.ticket(), bob.ticket());
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        served.unwrap();
        assert_eq!(synced.unwrap().rejected, 0);
        // A third member, on alice's node first: bob adopts it during the next session.
        let carol = Identity::load_or_create(&a.join("carol")).unwrap();
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.invite(&alice_id, &carol.public(), "member").unwrap();
        m.save(&a).unwrap();
        let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
        assert_eq!((served.unwrap().rejected, synced.unwrap().rejected), (0, 0));
        let (served, synced) = tokio::join!(bob.accept(), alice.sync(&bob_ticket));
        assert_eq!((served.unwrap().rejected, synced.unwrap().rejected), (0, 0));
        for h in [&a, &b] {
            assert!(sent.iter().all(|l| holds(h, borg::msg::FILE, l)), "every message kept");
        }
        assert_eq!(Membership::load(&b).unwrap().unwrap().members.len(), 3, "bob saw the change");
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
    fn older_protocols_are_every_version_below_the_spoken_ones() {
        let n = version(LEGACY_ALPN).expect("borg/mesh/<n>");
        assert_eq!(
            (version(V13_ALPN), version(V14_ALPN), version(ALPN)),
            (Some(n + 1), Some(n + 2), Some(n + 3)),
            "this protocol, 0.6's, 0.4.0's and 0.3.0's are the four spoken"
        );
        assert!(spoken(ALPN) && spoken(V14_ALPN) && spoken(V13_ALPN) && spoken(LEGACY_ALPN));
        assert_eq!((reads(ALPN), reads(V14_ALPN), reads(V13_ALPN), reads(LEGACY_ALPN)), (2, 2, 1, 0));
        let o = older();
        let below = format!("borg/mesh/{}", n - 1);
        assert_eq!((o.len(), o[0].as_slice(), o[o.len() - 1].as_slice()), (n as usize - 1, below.as_bytes(), b"borg/mesh/1".as_slice()));
        assert!(o.iter().all(|a| !spoken(a)));
        assert_eq!(synced(ALPN).filter(|f| *f != PLAYBOOKS).collect::<Vec<_>>(), SYNCED, "every synced log, in order");
        assert_eq!(synced(ALPN).collect::<Vec<_>>(), [&OLD_SYNCED[..], &[borg::msg::FILE, lines::ABSORBED]].concat(), "0.6's list, then the absorbs");
        assert_eq!(synced(V14_ALPN).collect::<Vec<_>>(), [&OLD_SYNCED[..], &[borg::msg::FILE]].concat(), "0.6's list, in its order");
        assert_eq!(synced(V13_ALPN).collect::<Vec<_>>(), synced(V14_ALPN).collect::<Vec<_>>(), "0.4.0's list is 0.6's");
        assert_eq!(synced(LEGACY_ALPN).collect::<Vec<_>>(), OLD_SYNCED, "0.3.0's list, in its order");
    }

    /// Bump rule (borg protocol.rs `NOTES`): this protocol says what an older member misses, and so does
    /// every spoken one above the oldest, so the announcement and `borg doctor` can name it.
    #[test]
    fn every_spoken_protocol_has_a_note() {
        let ours = version(ALPN).unwrap();
        assert_eq!(borg::protocol::current(), ours, "add borg/mesh/{ours} to borg protocol.rs NOTES, first");
        let oldest = version(LEGACY_ALPN).unwrap();
        for n in oldest + 1..=ours {
            let alpn = format!("borg/mesh/{n}");
            assert!(spoken(alpn.as_bytes()), "{alpn} is spoken");
            assert!(borg::protocol::note(n).is_some(), "{alpn} has a NOTES line");
        }
        assert_eq!(borg::protocol::announcement_head(ours), format!("{} is in use:", String::from_utf8_lossy(ALPN)));
    }

    /// A node older than 0.7 sends a hello without its version: it still parses, as `None`; a hello
    /// from this node names its version, and a node posing as an older one sends the old shape.
    #[test]
    fn a_hello_without_a_version_is_an_older_node() {
        let old: Msg = serde_json::from_str(r#"{"t":"hello","collective":"work","listen":null,"relay":null}"#).unwrap();
        assert!(matches!(old, Msg::Hello { borg: None, .. }));
        let older: Msg = serde_json::from_str(r#"{"t":"hello","collective":"work","listen":"127.0.0.1:1"}"#).unwrap();
        assert!(matches!(older, Msg::Hello { borg: None, relay: None, .. }), "before the relay too");
        let ours = Msg::Hello { collective: "work".into(), listen: None, relay: None, borg: Speaks::this().borg.map(str::to_string) };
        let text = serde_json::to_string(&ours).unwrap();
        assert!(text.contains(&format!(r#""borg":"{}""#, env!("CARGO_PKG_VERSION"))), "{text}");
        let posing = serde_json::to_string(&Msg::Hello { collective: "work".into(), listen: None, relay: None, borg: None }).unwrap();
        assert!(!posing.contains("borg"), "the old shape: {posing}");
        assert!(tells(None) && tells(Some("on")) && !tells(Some("off")), "BORG_ANNOUNCE=off turns it off");
    }

    /// The announcements in `home`'s message log of protocol `n`.
    fn announcements(home: &Path, n: u32) -> Vec<borg::msg::Message> {
        let head = borg::protocol::announcement_head(n);
        borg::msg::read(home).into_iter().filter(|m| m.text.starts_with(&head)).collect()
    }

    /// A session on an older protocol is announced once, as a tell to all from this node's engineer:
    /// not for a peer on this node's protocol, not while one is held, not when turned off, and again
    /// after a send the window refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_older_protocol_is_announced_once_and_retried_after_a_refusal() {
        let (pa, _) = team("announce");
        let a = home(&pa);
        let mut alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let ours = version(ALPN).unwrap();
        assert!(alice.announce(ALPN).is_none(), "a peer on this protocol");
        assert!(alice.announce(JOIN_ALPN).is_none(), "no session protocol");
        alice.speaks.tell = false;
        assert!(alice.announce(V14_ALPN).is_none(), "BORG_ANNOUNCE=off");
        alice.speaks.tell = true;

        // The window is full: refused, nothing written, and the next session tries again.
        let from = borg::msg::Sender { by: borg::msg::ENGINEER.into(), name: borg::msg::ENGINEER.into(), harness: "terminal".into(), node: alice.public() };
        let now = borg::learn::now_ms();
        let mut filled = 0;
        while borg::msg::send(&a, borg::msg::Message::new(borg::msg::TELL, &from, ALL_TO, None, now + filled, &format!("note {filled}")), now + filled).is_ok()
        {
            filled += 1;
            assert!(filled < 1000, "the window bounds a sender");
        }
        assert!(alice.announce(V14_ALPN).is_none(), "refused by the window");
        assert!(announcements(&a, ours).is_empty());
        std::fs::remove_file(borg::msg::path(&a)).unwrap(); // read, expired: the window is open again

        // The engineer told all a minute ago and a drone saw it: the window takes another, but a 0.4 or
        // 0.5 member would be offered that tell alone (`v1_window`), so the announcement waits.
        let freeze = borg::msg::send(&a, borg::msg::Message::new(borg::msg::TELL, &from, ALL_TO, None, now - 60_000, "freeze"), now - 60_000).unwrap();
        borg::msg::ack(&a, "s1", &[&freeze.id], now - 30_000).unwrap();
        assert!(alice.announce(V13_ALPN).is_none(), "the engineer's v1 slot to all is held");
        assert!(announcements(&a, ours).is_empty());
        std::fs::remove_file(borg::msg::path(&a)).unwrap();

        let sent = alice.announce(V13_ALPN).expect("announced");
        assert_eq!(
            (sent.kind, sent.to.as_str(), sent.by.as_str(), sent.node.as_str()),
            (borg::msg::TELL, ALL_TO, borg::msg::ENGINEER, alice.public().as_str())
        );
        assert_eq!(sent.v, 1, "a 0.4 or 0.5 node reads it: {}", sent.text);
        for missed in [14, 15] {
            assert!(sent.text.contains(borg::protocol::note(missed).unwrap()), "{}", sent.text);
        }
        assert!(sent.text.contains(env!("CARGO_PKG_VERSION")) && sent.text.contains("borg update"), "{}", sent.text);
        assert!(alice.announce(V14_ALPN).is_none() && alice.announce(V13_ALPN).is_none(), "held: once");
        assert_eq!(announcements(&a, ours).len(), 1);
        alice.close().await;
    }

    /// Sessions with older members that end together under `serve` announce once between them.
    #[tokio::test(flavor = "multi_thread")]
    async fn sessions_that_end_together_announce_once() {
        let (pa, _) = team("announce-race");
        let a = home(&pa);
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let ours = version(ALPN).unwrap();
        // A tell a drone saw, from before the v1 slot: the window would take two announcements.
        let from = borg::msg::Sender { by: borg::msg::ENGINEER.into(), name: borg::msg::ENGINEER.into(), harness: "terminal".into(), node: alice.public() };
        let then = borg::learn::now_ms() - V1_RATE_MS - 60_000;
        let early = borg::msg::send(&a, borg::msg::Message::new(borg::msg::TELL, &from, ALL_TO, None, then, "early"), then).unwrap();
        borg::msg::ack(&a, "s1", &[&early.id], then + 1).unwrap();
        let go = std::sync::Barrier::new(8);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    go.wait();
                    alice.announce(V14_ALPN)
                });
            }
        });
        assert_eq!(announcements(&a, ours).len(), 1);
        alice.close().await;
    }

    /// A newer member that no longer speaks this node's protocol agrees on it at the handshake and closes
    /// the session (its `mismatch`): this node records it from the close reason, both ways, so `borg
    /// doctor`, the views and the drone notice say that this node has to upgrade.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_on_a_newer_protocol_is_recorded_from_its_refusal() {
        let (pa, pb) = team("newer");
        let (a, b) = (home(&pa), home(&pb));
        let bob_key = Identity::load_or_create(&b).unwrap().public();
        let ours = String::from_utf8_lossy(ALPN).into_owned();
        let newer = format!("borg/mesh/{}", version(ALPN).unwrap() + 1);
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        // Bob speaks only the newer protocol and accepts this one only to tell the mismatch.
        let new_bob = Node::open(&pb, Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![newer.clone().into_bytes(), ALPN.to_vec()])).await.unwrap();
        let told = borg::presence::Mismatch { theirs: ours.clone(), ours: newer.clone(), ms: 1 }.told(&bob_key);
        let line = format!("peer {} speaks {newer}, this node {ours}: nothing syncs with it; upgrade this node", &bob_key[..12]);

        // Alice dials the newer node.
        let refuse = async {
            let conn = new_bob.incoming().await?.await.ok()?;
            let agreed = conn.alpn().to_vec();
            conn.close(MISMATCH_CLOSE.into(), told.as_bytes());
            Some(agreed)
        };
        let ticket = new_bob.ticket();
        let (agreed, synced) = tokio::join!(refuse, alice.sync(&ticket));
        assert_eq!(agreed.as_deref(), Some(ALPN), "the handshake agreed on this node's protocol");
        assert_eq!(synced.unwrap_err(), line);
        let m = &borg::presence::mismatches(&a)[&bob_key];
        assert_eq!((m.theirs.as_str(), m.ours.as_str()), (newer.as_str(), ours.as_str()));
        let shown = a.join("shown");
        let n = borg::update::notice(None, &a, &shown).expect("the drone hears it");
        assert!(n.contains(&format!("Nothing syncs with peer {} ({newer}", &bob_key[..12])), "{n}");

        // The newer node dials alice.
        let _ = std::fs::remove_file(borg::presence::protocols_path(&a));
        let addr = Target::parse(&alice.ticket()).unwrap().endpoint_addr(&new_bob.relay).unwrap();
        let dial_new = async {
            let opts = ConnectOptions::new().with_additional_alpns(vec![ALPN.to_vec()]);
            let conn = new_bob.endpoint.connect_with_opts(addr, newer.as_bytes(), opts).await.map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
            let agreed = conn.alpn().to_vec();
            conn.close(MISMATCH_CLOSE.into(), told.as_bytes());
            Ok::<Vec<u8>, String>(agreed)
        };
        let (served, dialled) = tokio::join!(alice.accept(), dial_new);
        assert_eq!(dialled.unwrap(), ALPN);
        assert_eq!(served.unwrap_err(), line);
        assert_eq!(borg::presence::mismatches(&a)[&bob_key].theirs, newer);
        // A close for any other reason records nothing.
        assert!(borg::presence::Mismatch::from_told("error", 1).is_none());
        alice.close().await;
        new_bob.close().await;
    }

    /// A 0.6 node (`borg/mesh/14`, no version in its hello) and this one sync: each records the other's
    /// version and protocol, this node announces its protocol once, the announcement reaches the older
    /// node at the next session, and no session announces again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_on_an_older_protocol_hears_the_announcement_once() {
        let (pa, pb) = team("announce-sim");
        let (a, b) = (home(&pa), home(&pb));
        let (alice_key, bob_key) = (Identity::load_or_create(&a).unwrap().public(), Identity::load_or_create(&b).unwrap().public());
        let (ours, theirs) = (version(ALPN).unwrap(), version(V14_ALPN).unwrap());
        let alice = Node::bind(&pa, LOCAL.parse().unwrap()).await.unwrap();
        let mut old_bob = Node::open(&pb, Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![V14_ALPN.to_vec()])).await.unwrap();
        old_bob.speaks = Speaks { alpn: V14_ALPN, borg: None, tell: true };

        // The old node dials: this node announces once its session completed.
        let ticket = alice.ticket();
        let (served, dialled) = tokio::join!(alice.accept(), old_bob.dial(&ticket, &[], V14_ALPN, Vec::new()));
        served.unwrap();
        dialled.unwrap();
        let held = announcements(&a, ours);
        assert_eq!(held.len(), 1, "one announcement");
        assert!(held[0].text.contains(borg::protocol::note(ours).unwrap()), "{}", held[0].text);
        assert!(announcements(&b, ours).is_empty() && announcements(&b, theirs).is_empty(), "the old node announces nothing");
        let v = &borg::protocol::versions(&a)[&bob_key];
        assert_eq!((v.borg.as_deref(), v.protocol), (None, theirs), "no version: older than 0.7");
        let v = &borg::protocol::versions(&b)[&alice_key];
        assert_eq!((v.borg.as_deref(), v.protocol), (Some(env!("CARGO_PKG_VERSION")), theirs));

        // This node dials: the announcement reaches the old node, and nothing is announced again.
        let old_ticket = old_bob.ticket();
        for _ in 0..2 {
            let (served, dialled) = tokio::join!(old_bob.accept(), alice.sync(&old_ticket));
            served.unwrap();
            dialled.unwrap();
        }
        assert_eq!(announcements(&a, ours), held, "still the one");
        assert_eq!(announcements(&b, ours), held, "the old node holds it, signed by this node");
        alice.close().await;
        old_bob.close().await;
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

    /// Carol is a member alice dials, and she takes the dial and never answers: she reads alice's hello
    /// and holds the connection until the test lets her go. While alice's dial to carol is still open,
    /// a line alice writes reaches bob, whom only alice's dials reach. Once carol closes, alice's dial
    /// fails and carol's session record from before goes with it. Ordered by events, not by time:
    /// the dial timeout is far past the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_does_not_answer_holds_no_line_back() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::oneshot;
        let p = crew("deaf", &["alice", "bob", "carol"]);
        let (a, b) = (home(&p[0]), home(&p[1]));
        let alice = Arc::new(Node::bind(&p[0], LOCAL.parse().unwrap()).await.unwrap());
        let bob = Arc::new(Node::bind(&p[1], LOCAL.parse().unwrap()).await.unwrap());
        let carol = Node::bind(&p[2], LOCAL.parse().unwrap()).await.unwrap();
        let key = carol.public();
        assert!(peers::remember(&a, &key, &addr_of(&carol)).unwrap());
        assert!(peers::remember(&a, &bob.public(), &addr_of(&bob)).unwrap());
        borg::presence::note_synced(&a, &key, borg::learn::now_ms()).unwrap();
        let (held_tx, held) = oneshot::channel::<()>();
        let (release, release_rx) = oneshot::channel::<()>();
        let deaf = tokio::spawn(async move {
            let conn = carol.incoming().await.expect("alice dials carol").await.expect("handshake");
            let (_tx, mut rx) = conn.accept_bi().await.expect("alice's stream");
            assert!(matches!(recv(&mut rx).await, Ok(Msg::Hello { .. })), "alice's hello");
            let _ = held_tx.send(());
            let _ = release_rx.await;
            let open = conn.close_reason().is_none();
            conn.close(1u32.into(), b"deaf");
            carol.close().await;
            open
        });
        let opts =
            crate::daemon::Options { poll: Duration::from_millis(100), timeout: Duration::from_secs(600), grace: Duration::from_secs(1), ..Default::default() };
        let serve = serving(vec![("alice", alice.clone()), ("bob", bob.clone())], opts);
        let limit = Duration::from_secs(60); // liveness only: every wait below ends on an event
        tokio::time::timeout(limit, held).await.expect("carol holds alice's dial").unwrap();

        let l = rollup("n1", 1, 1);
        append_lines(&a, "stats.jsonl", std::slice::from_ref(&l));
        eventually("alice's line at bob", limit, || holds(&b, "stats.jsonl", &l)).await;
        assert!(serve.failed.lock().unwrap().iter().all(|f| !f.contains(&key)), "carol's dial has not ended");
        release.send(()).unwrap();
        assert!(deaf.await.unwrap(), "alice's dial to carol was still open when bob got the line");

        eventually("alice's dial to carol failed", limit, || serve.failed.lock().unwrap().iter().any(|f| f.contains(&key))).await;
        eventually("carol's record dropped", limit, || !borg::presence::synced(&a).contains_key(&key)).await;
        serve.stop().await;
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

    /// The logs a 0.3.0 node reconciled, in its order (base 188c262 lines.rs `SYNCED`).
    const OLD_SYNCED: [&str; 5] = ["lessons.jsonl", "stats.jsonl", "outcomes-team.jsonl", "playbooks.jsonl", "constraints.jsonl"];

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
    /// and `Live` with its hive beats and a signed directive. Returns the hive lines it gave and the
    /// presence it took in.
    async fn old_exchange(n: &Node, dialler: bool, tx: &mut SendStream, rx: &mut RecvStream, goal: &OldGoal) -> Result<(usize, usize), String> {
        let home = n.home.as_path();
        let mine = members(home)?;
        let Msg::Membership { .. } = swap(dialler, tx, rx, Msg::Membership { docs: Membership::history(home) }).await? else { return Err("membership".into()) };
        n.stamp()?;
        let (origins, mut report) = (origin::load(home), Report::default());
        for file in OLD_SYNCED {
            n.reconcile(dialler, tx, rx, &home.join(file), &origins, &mine, false, &mut report).await?;
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
        let directive = json!({"id": "d1", "collective": "work", "key": n.me.public(), "engineer": "old@x", "target": {"node": "", "session": ""},
            "kind": "pause", "text": "", "ms": now, "sig": "00"});
        let live = json!({"t": "live", "presence": live::outgoing(home, &n.me, now), "directives": [directive],
            "beats": [{"goal": "g1", "line": "{}", "key": n.me.public(), "sig": "00"}]});
        let theirs = if dialler {
            send_raw(tx, &live).await?;
            recv(rx).await?
        } else {
            let theirs = recv(rx).await?;
            send_raw(tx, &live).await?;
            theirs
        };
        let Msg::Live { presence, .. } = theirs else { return Err("live".into()) };
        Ok((gave, live::receive(home, &mine, &n.me.public(), presence, now)?))
    }

    async fn old_dial(n: &Node, target: &str, goal: &OldGoal) -> Result<(usize, usize), String> {
        let t = Target::parse(target)?;
        let conn = n.endpoint.connect(t.endpoint_addr(&n.relay)?, LEGACY_ALPN).await.map_err(|e| e.to_string())?;
        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
        swap(true, &mut tx, &mut rx, Msg::Hello { collective: "work".into(), listen: None, relay: None, borg: None }).await?;
        let got = old_exchange(n, true, &mut tx, &mut rx, goal).await;
        let _ = tx.finish();
        conn.close(0u32.into(), b"done");
        got
    }

    async fn old_serve(n: &Node, goal: &OldGoal) -> Result<(usize, usize), String> {
        let conn = n.incoming().await.ok_or("closed")?.await.map_err(|e| e.to_string())?;
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(|e| e.to_string())?;
        let Msg::Hello { .. } = recv(&mut rx).await? else { return Err("hello".into()) };
        send(&mut tx, &Msg::Hello { collective: "work".into(), listen: None, relay: None, borg: None }).await?;
        let got = old_exchange(n, false, &mut tx, &mut rx, goal).await;
        let _ = tx.finish();
        conn.closed().await;
        got
    }

    /// A 0.3.0 peer (it names hive goals, reconciles their logs and the playbooks, and sends beats in
    /// `Live`) keeps syncing with this node in both roles: lessons and presence arrive both ways, its
    /// directives are ignored, no hive or playbooks file is written, and a hive line it gave once is not
    /// sent again when this node dials.
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
        let edit = json!({"pb": "p1", "edit": "e1", "step": "s1", "text": "run the parser tests", "ms": 1}).to_string();
        write(&a, PLAYBOOKS, std::slice::from_ref(&edit));
        origin::append(&a, &[Origin::new(&alice_id, &lines::hash(&edit), 1)]).unwrap();
        let now = borg::learn::now_ms();
        for (h, p, who) in [(&a, &pa, "alice"), (&b, &pb, "bob")] {
            let ev = json!({"session_id": format!("{who}-s1"), "cwd": p.dir, "prompt": format!("{who} works")});
            borg::presence::write(h, &borg::presence::update(None, "work", "claude", "prompt", &ev, now, |_| (format!("{who}@x"), "repo".into()))).unwrap();
        }
        let tell = message(&b, &bob_id.public(), ALL_TO, now, "the new node's note");
        let old = Node::open(&pa, Some(LOCAL.parse().unwrap()), true, |x| x.alpns(vec![LEGACY_ALPN.to_vec()])).await.unwrap();
        let new = Node::bind(&pb, LOCAL.parse().unwrap()).await.unwrap();
        let (old_at, new_at) = (old.ticket(), new.ticket());
        let mut goal = OldGoal::new(&alice_id);

        // The new node dials: every hive line comes once and is dropped.
        let (old_got, new_got) = tokio::join!(old_serve(&old, &goal), new.sync(&old_at));
        let (new_got, (gave, presence)) = (new_got.unwrap(), old_got.unwrap());
        assert_eq!((gave, presence, new_got.presence), (LEGACY_GOAL_LOGS, 1, 1));
        assert!(holds(&a, "lessons.jsonl", &l2) && holds(&b, "lessons.jsonl", &l1), "lessons both ways");

        // The old node dials: its directive (bob may steer) is ignored, and the listener's echo moves
        // no hive line.
        let (old_got, new_got) = tokio::join!(old_dial(&old, &new_at, &goal), new.accept());
        let (new_got, (gave, _)) = (new_got.unwrap(), old_got.unwrap());
        assert_eq!((gave, new_got.steered, new_got.dropped), (0, 0, 0));
        assert!(!borg::steer::active(&b), "a 0.3.0 directive counts nowhere");

        // The new node dials again: only the line added since comes.
        goal.add(&alice_id, 5, "{\"bundle\":\"new\"}");
        let (old_got, new_got) = tokio::join!(old_serve(&old, &goal), new.sync(&old_at));
        new_got.unwrap();
        assert_eq!(old_got.unwrap().0, 1, "what was given before is not sent again");
        assert!(!b.join("hive").exists(), "no hive file on the new node");
        assert!(!b.join(PLAYBOOKS).exists(), "the old node's playbook edit is followed, not kept");
        assert!(holds(&b, borg::msg::FILE, &tell) && !a.join(borg::msg::FILE).exists(), "messages stay off a 0.3.0 session");
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

    /// A steer from the node whose member key is `node` (its engineer at a terminal) to `to`.
    fn steering(node: &str, kind: borg::msg::Kind, to: &str, ms: i64, text: &str) -> borg::msg::Message {
        let from = borg::msg::Sender { by: "engineer:bob@x".into(), name: "bob@x".into(), harness: "terminal".into(), node: node.into() };
        borg::msg::Message::new(kind, &from, to, None, ms, text)
    }

    /// Presence and steering ride the session: bob (role steer) sees alice's drone and pauses it, a
    /// message line whose origin is bob's node; alice admits it under bob's role and her hook refuses
    /// tools until bob's resume, sent at once after. An observer's pause is stored but refused and
    /// logged; once bob is revoked, his pause no longer holds.
    #[tokio::test(flavor = "multi_thread")]
    async fn presence_and_steering_travel_between_members() {
        use borg::msg::Kind;
        let (pa, pb) = team("live");
        let (a, b) = (home(&pa), home(&pb));
        let (alice_id, bob_id) = (Identity::load_or_create(&a).unwrap(), Identity::load_or_create(&b).unwrap());
        let (alice_key, bob_key) = (alice_id.public(), bob_id.public());
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.invite(&alice_id, &bob_key, "steer").unwrap();
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
        assert_eq!((ds.len(), ds[0].node.as_str(), ds[0].p.prompt.as_str()), (1, alice_key.as_str(), "refactor the recon loop"));
        let lessons = std::fs::read_to_string(b.join("lessons.jsonl")).unwrap_or_default();
        assert!(!lessons.contains("recon loop"), "never in lessons");

        let to = format!("alice-s1@{alice_key}");
        let pre = json!({"session_id": "alice-s1", "cwd": pa.dir, "tool_name": "Bash"});
        let refused = |at: i64| borg::steer::around(&a, "pre_tool", &pre, Some(now), at, || None);
        let round = || async {
            let (served, synced) = tokio::join!(alice.accept(), bob.sync(&ticket));
            synced.unwrap();
            served.unwrap()
        };
        borg::msg::send(&b, steering(&bob_key, Kind::Pause, &to, now, ""), now).unwrap();
        let served = round().await;
        assert_eq!((served.steered, served.dropped), (1, 0));
        let deny = refused(now + 1).expect("paused");
        let why = deny["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
        assert!(why.contains(&format!("paused by bob@x (node {}, role steer)", &bob_key[..12])), "{why}");

        // An observer's pause, handed over signed by its node: stored, refused, logged.
        let by_observer = serde_json::to_string(&steering(&observer.public(), Kind::Pause, ALL_TO, now, "")).unwrap();
        let h = lines::hash(&by_observer);
        let mut report = Report::default();
        let got = vec![Signed { origin: Origin::new(&observer, &h, now), line: by_observer.clone() }];
        alice.store_lines(&a.join(borg::msg::FILE), got, &BTreeSet::from([h]), &members(&a).unwrap(), None, &mut report).unwrap();
        assert_eq!((report.received, report.steered, report.dropped), (1, 0, 1));
        let log = std::fs::read_to_string(borg::steer::dropped_path(&a)).unwrap();
        assert!(log.contains("role observe may not steer"), "{log}");

        // Bob's resume, a moment after his pause: not paced away, and it lifts the pause.
        borg::msg::send(&b, steering(&bob_key, Kind::Resume, &to, now + 2, ""), now + 2).unwrap();
        assert_eq!(round().await.steered, 1);
        assert_eq!(refused(now + 3), None);

        // Bob pauses again, then is revoked: his pause stops holding at alice.
        borg::msg::send(&b, steering(&bob_key, Kind::Pause, &to, now + 4, ""), now + 4).unwrap();
        assert_eq!(round().await.steered, 1);
        assert!(refused(now + 5).is_some(), "paused again");
        let mut m = Membership::load(&a).unwrap().unwrap();
        m.revoke(&alice_id, &bob_key, now + 5).unwrap();
        m.save(&a).unwrap();
        alice.enforce().unwrap();
        assert!(borg::steer::read_admitted(&a).iter().all(|x| x.node != bob_key), "the revoked node's admissions are gone");
        assert_eq!(refused(now + 6), None, "and the pause no longer holds");
        alice.close().await;
        bob.close().await;
    }

    /// `borg no` on bob's node for alice's drone: a veto message, admitted and recorded once on alice's
    /// node (credit, episode, stat), shown to her drone once, and back on bob's node as a team outcome,
    /// so both nodes' beliefs count the veto once.
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

        let to = format!("alice-s1@{}", alice_id.public());
        let veto = steering(&bob_id.public(), borg::msg::Kind::Veto, &to, now, "never look merges up in the commit log");
        borg::msg::send(&b, veto, now).unwrap();
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
        // Alice's drone is told once, labelled with bob's node and role.
        let caps = borg::harness::by_name("claude").unwrap().caps();
        let prompt = json!({"session_id": "alice-s1", "cwd": pa.dir});
        let out = borg::msg::deliver(&a, caps, "prompt", &prompt, Some(now - 10), now + 1, None).expect("shown");
        let ctx = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.contains(&format!("[borg] Veto from bob@x (node {}, role steer)", &bob_id.public()[..12])), "{ctx}");
        assert_eq!(borg::msg::deliver(&a, caps, "prompt", &prompt, Some(now - 10), now + 2, None), None, "once");
        // Both nodes' beliefs count it once: alice's from her own record, bob's from the team outcome.
        let l1 = [borg::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let belief = |h: &Path| borg::memory::derive(&borg::outcome::read_team(&h.join("lessons.jsonl")), &l1, None)["L1"].vetoed;
        assert_eq!((belief(&a), belief(&b)), (1, 1));
        assert!(!b.join("outcomes.jsonl").exists() && !b.join("episodes.jsonl").exists(), "bob's node holds only the team copy");
        alice.close().await;
        bob.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_collectives_presence_and_steering_never_leave() {
        let p = node("live-private", "local");
        let h = home(&p);
        let now = borg::learn::now_ms();
        let ev = json!({"session_id": "s1", "cwd": p.dir, "prompt": "private diary"});
        borg::presence::write(&h, &borg::presence::update(None, "work", "claude", "prompt", &ev, now, |_| ("me@x".into(), "r".into()))).unwrap();
        let me = Identity::load_or_create(&h).unwrap().public();
        borg::msg::send(&h, steering(&me, borg::msg::Kind::Pause, ALL_TO, now, "lunch"), now).unwrap();
        let held = borg::steer::read_admitted(&h);
        assert_eq!((held.len(), held[0].role), (1, borg::steer::Role::Local), "this machine's engineer steers it");
        assert!(Node::bind(&p, LOCAL.parse().unwrap()).await.is_err(), "no socket at all");
    }

    /// A pause and its resume from one sender to one session both travel at once (send does not pace a
    /// steer). Of more unread tells from one sender to one recipient within `V1_RATE_MS` than any window
    /// lets through (`WINDOW_CAP`), the oldest are outside; one acknowledged is in, and so is one a rate
    /// slot later.
    #[test]
    fn the_window_bounds_a_burst_and_never_a_steer() {
        use borg::msg::{Kind, Seen, V1_RATE_MS, WINDOW_CAP};
        let (node, recipient) = ("a".repeat(64), "b".repeat(64));
        let to = format!("s1@{recipient}");
        let steers = [steering(&node, Kind::Pause, &to, 1000, ""), steering(&node, Kind::Resume, &to, 1001, "")];
        let burst: Vec<borg::msg::Message> = (0..WINDOW_CAP + 2).map(|i| steering(&node, Kind::Tell, &to, 2000 + i as i64, &format!("t{i}"))).collect();
        let later = steering(&node, Kind::Tell, &to, 2000 + burst.len() as i64 + V1_RATE_MS, "a slot later");
        let line = |m: &borg::msg::Message| serde_json::to_string(m).unwrap();
        let mut set: BTreeMap<Hash, String> = steers.iter().chain(&burst).chain([&later]).map(line).map(|l| (lines::hash(&l), l)).collect();
        let within = |set: &BTreeMap<Hash, String>| {
            let w = window(set);
            steers.iter().chain(&burst).chain([&later]).map(|m| w.contains(&lines::hash(&line(m)))).collect::<Vec<bool>>()
        };
        let mut want = vec![true; steers.len() + burst.len() + 1];
        (want[2], want[3]) = (false, false);
        assert_eq!(within(&set), want, "steers in; of {} unread within a rate slot, the newest {WINDOW_CAP}", burst.len());
        let seen = serde_json::to_string(&Seen::new("s1", &recipient, 3000, &[&burst[0].id])).unwrap();
        set.insert(lines::hash(&seen), seen);
        want[2] = true;
        assert_eq!(within(&set), want, "the first acknowledged: in, and the second still one too many");
    }

    /// What a `borg/mesh/13` peer is offered: a pause and then `per_node + 1` newer tells from its node
    /// keep the pause (steers have their own share, as 0.5.0 kept them), the oldest tell goes, and no v2
    /// text or seen line is offered at all.
    #[test]
    fn the_v1_window_keeps_a_pause_and_offers_only_v1_lines() {
        use borg::msg::Kind;
        let (node, per) = ("a".repeat(64), 5);
        let to = |i: usize| format!("s{i}@{}", "b".repeat(64));
        let ms: Vec<borg::msg::Message> = std::iter::once(steering(&node, Kind::Pause, &to(0), 1000, ""))
            .chain((0..=per).map(|i| steering(&node, Kind::Tell, &to(i), 1001 + i as i64, "hi")))
            .collect();
        let long = steering(&node, Kind::Tell, &to(9), 2000, &"w".repeat(borg::msg::V1_TEXT + 1));
        let seen = seen_of(&node, "s1", 2001, &[&ms[1].id]);
        let mut set: BTreeMap<Hash, String> = ms.iter().chain([&long]).map(|m| serde_json::to_string(m).unwrap()).map(|l| (lines::hash(&l), l)).collect();
        set.insert(lines::hash(&seen), seen.clone());
        let w = v1_window(&set, per);
        let kept = |m: &borg::msg::Message| w.contains(&lines::hash(&serde_json::to_string(m).unwrap()));
        assert!(kept(&ms[0]), "the pause travels");
        assert_eq!((kept(&ms[1]), ms[2..].iter().filter(|m| kept(m)).count()), (false, per), "the oldest tell went");
        assert!(!kept(&long) && !w.contains(&lines::hash(&seen)), "a v2 text and a seen line are not 0.5.0's to read");
    }

    /// What `send` writes is within the mesh's window, on every node and however much of the log the TTL
    /// and the bound have pruned since: a log built by `send` and `ack` alone (sessions of this node and
    /// of another, a broadcast and the engineer; some acknowledged, some answered, some refused, over
    /// three TTLs), each tell in it within `window`, also over every later stretch of it, as a node
    /// holding only that stretch sees it. Not vacuous: over those stretches `window_for` admits less than
    /// `send` did.
    #[test]
    fn what_send_wrote_stays_within_the_window_however_the_log_is_pruned() {
        use borg::msg::{Address, Kind, Message, SendError, Sender};
        let home = std::env::temp_dir().join(format!("borg-mesh-sent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let here = borg::msg::node(&home);
        let senders = ["s1", "s2", "engineer:e@x"];
        let recipients = [format!("r1@{here}"), format!("r1@{here}"), format!("r2@{here}"), format!("r3@{}", "d".repeat(64)), "all".into(), "engineer".into()];
        let mut seed = 11u64;
        let mut pick = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        let start = 10 * borg::msg::TTL_MS;
        let (mut ms, mut sent, mut acks, mut refused) = (start, Vec::<Message>::new(), 0, 0);
        while ms < start + 3 * borg::msg::TTL_MS {
            ms += [1_000, 1_000, borg::msg::V1_RATE_MS, 3_600_000, 6 * 3_600_000][pick(5)];
            let from = Sender { by: senders[pick(senders.len())].into(), name: "One of One".into(), harness: "claude".into(), node: here.clone() };
            // One of the latest, so acknowledgements and answers open windows still in use.
            let earlier = (!sent.is_empty()).then(|| sent[sent.len() - 1 - pick(sent.len().min(4))].clone());
            // Its recipient here shown it (a broadcast: any other session here), or answering it.
            let recipient = earlier.as_ref().and_then(|m| match Address::parse(&m.to) {
                Some(Address::Session { id, node: Some(n) }) if n == here => Some(id),
                Some(Address::All) => Some("r9".to_string()),
                _ => None,
            });
            match (pick(6), earlier, recipient) {
                (0 | 1, Some(m), Some(who)) => {
                    borg::msg::ack(&home, &who, &[&m.id], ms).unwrap();
                    acks += 1;
                }
                (2, Some(m), Some(who)) if !borg::msg::is_engineer(&m.by) => {
                    let from = Sender { by: who, name: "Two of Two".into(), harness: "omp".into(), node: here.clone() };
                    let _ = borg::msg::send(&home, Message::new(Kind::Tell, &from, &format!("{}@{here}", m.by), Some(&m.id), ms, "re"), ms);
                }
                _ => match borg::msg::send(&home, Message::new(Kind::Tell, &from, &recipients[pick(recipients.len())], None, ms, "hi"), ms) {
                    Ok(m) => sent.push(m),
                    Err(SendError::Window { .. }) => refused += 1,
                    Err(e) => panic!("{e}"),
                },
            }
        }
        let held: Vec<String> = std::fs::read_to_string(borg::msg::path(&home)).unwrap().lines().map(String::from).collect();
        let talk = |l: &str| Line::parse(l).and_then(|x| x.message().cloned()).filter(|m| !m.kind.privileged());
        assert!(acks > 10 && refused > 10 && held.iter().filter(|l| talk(l).is_some()).count() < sent.len(), "{acks} acks, {refused} refused, pruned");
        let mut fewer = 0;
        for cut in (0..held.len()).step_by(5) {
            let set: BTreeMap<Hash, String> = held[cut..].iter().map(|l| (lines::hash(l), l.clone())).collect();
            let (w, parsed) = (window(&set), held[cut..].iter().filter_map(|l| Line::parse(l)).collect::<Vec<Line>>());
            for (l, m) in held[cut..].iter().filter_map(|l| talk(l).map(|m| (l, m))) {
                assert!(w.contains(&lines::hash(l)), "from line {cut} on: {l}");
                fewer += usize::from(!borg::msg::window_for(&parsed, m.author(), &m.to, ms).admits(&m.id));
            }
        }
        assert!(fewer > 0, "some stretch's replay admits less than send did");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `Live` still carries the `directives` a 0.3.0 peer requires, always empty, and takes whatever
    /// such a peer puts there without reading it.
    #[test]
    fn live_keeps_an_empty_directives_field_for_0_3_0() {
        let out = serde_json::to_value(Msg::Live { presence: Vec::new(), directives: Directives }).unwrap();
        assert_eq!(out, json!({"t": "live", "presence": [], "directives": []}));
        let theirs = json!({"t": "live", "presence": [], "directives": [{"kind": "pause", "sig": "00"}], "beats": []});
        assert!(matches!(serde_json::from_value::<Msg>(theirs).unwrap(), Msg::Live { .. }));
        assert!(matches!(serde_json::from_value::<Msg>(json!({"t": "live", "presence": []})).unwrap(), Msg::Live { .. }));
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
