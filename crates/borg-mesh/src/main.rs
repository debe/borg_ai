//! borg-mesh: sync a collective's lessons with its members, peer to peer (`borg mesh ...` runs this).
//!
//! usage: borg-mesh id                         (this node's public key in the collective)
//!        borg-mesh init                       (found the collective's membership; this node is admin)
//!        borg-mesh invite [--role member|steer|observe] [--expires 7d] [--uses 1] [--png FILE]
//!                                             (admin: print a ticket and its QR code; invite.rs)
//!        borg-mesh invite <key> [--role R]    (admin: add or change a member by key, re-sign)
//!        borg-mesh invites [--revoke ID]      (admin: the tickets still usable; withdraw one)
//!        borg-mesh join <ticket> [--paths DIR...] [--apply]
//!                                             (new member: add the collective to collectives.json with the
//!                                              repo dirs DIR, a placeholder without them (dry-run diff
//!                                              first), then dial the admin, join, sync)
//!        borg-mesh join <file>                (install the membership.json, or membership-history.jsonl,
//!                                              the admin handed over)
//!        borg-mesh revoke <key>               (admin: remove a member; spreads with the next sync)
//!        borg-mesh members                    (who belongs, who was revoked)
//!        borg-mesh peer <key>[@<ip:port>|<relay url>,...]
//!                                             (where a member can be reached; `peer` alone lists them)
//!        borg-mesh serve [--bind IP:PORT] [--interval SECS]
//!                                             (run until Ctrl-C / SIGTERM: answer members, sync with
//!                                              every known peer each interval and on local change;
//!                                              default 0.0.0.0:7878, 60 s)
//!        borg-mesh sync <key>[@<hints>]       (reconcile with one member, once)
//!        borg-mesh steer <drone|session|all> "<text>" | <drone> --pause | --resume | --veto "<reason>"
//!                                             (signed steering, steer.rs; `borg steer` runs this)
//!        borg-mesh fetch [<commit>...]        (ask every known member for the reasoning excerpts of these
//!                                              commits, default: recent commits without a note, and
//!                                              write them as git notes; `borg notes fetch` runs this)
//! Every command takes `--dir DIR` (default: the current directory): the collective is the one DIR
//! routes to (collective.rs); or `--collective NAME` names one listed in collectives.json. An off
//! repo or a local collective is refused by every networked command.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use borg::collective::Route;
use borg_mesh::addr::Relay;
use borg_mesh::daemon::{self, Event, Options};
use borg_mesh::identity::Identity;
use borg_mesh::invite::{self, Ticket, join_config};
use borg_mesh::membership::{self, Membership};
use borg_mesh::sync::{Node, Place, Report};
use borg_mesh::{origin, peers};

fn show(r: &Report) -> String {
    format!(
        "sent {}, received {}, refused {}, quarantined {}, replicas handed over {} ({} rounds, {} fingerprints, {} hashes)",
        r.sent, r.received, r.rejected, r.quarantined, r.replicated, r.rounds, r.fps, r.hashes
    )
}

fn short(key: &str) -> &str {
    key.get(..12).unwrap_or(key)
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn place(args: &[String]) -> Place {
    let dir = flag(args, "--dir").map(str::to_string).unwrap_or_else(|| std::env::current_dir().unwrap_or_default().display().to_string());
    let p = Place::from_env(&dir);
    match flag(args, "--collective") {
        Some(name) => p.named(name),
        None => p,
    }
}

/// The first argument after the command, unless it is a flag.
fn positional(args: &[String]) -> Option<&str> {
    args.get(1).map(String::as_str).filter(|a| !a.starts_with("--"))
}

/// `invite` without a key: a ticket for whoever presents it first (invite.rs).
fn invite_ticket(place: &Place, args: &[String]) -> Result<(), String> {
    let (name, home) = place.gate()?;
    let m = Membership::load(&home)?.ok_or(format!("{name} has no membership: `borg mesh init`"))?;
    let role = flag(args, "--role").unwrap_or("member");
    let ttl = invite::duration_ms(flag(args, "--expires").unwrap_or("7d"))?;
    let uses = flag(args, "--uses").unwrap_or("1").parse::<u32>().map_err(|e| format!("--uses: {e}"))?;
    let relay = place.relay()?;
    let hints = invite::load_reach(&home);
    if relay == Relay::Disabled && hints.is_empty() {
        return Err("relays are disabled and no address of this node is known: start `borg mesh serve` once, then invite".into());
    }
    let now = borg::learn::now_ms();
    let inv = invite::issue(&home, &Identity::load_or_create(&home)?, &m, role, ttl, uses, now)?;
    let ticket = Ticket { invite: inv.clone(), relay, hints }.encode();
    println!("{}\n", invite::qr_text(&ticket)?);
    println!("{ticket}\n");
    let plural = if uses == 1 { "" } else { "s" };
    println!("invite {} to {name}: role {role}, {uses} use{plural}, expires {}", &inv.id[..8], borg::install::utc(inv.expires_ms));
    println!("the new member runs:  borg mesh join <ticket>   (or scans the QR code and pastes it)");
    if let Some(png) = flag(args, "--png") {
        std::fs::write(png, invite::qr_png(&ticket)?).map_err(|e| format!("{png}: {e}"))?;
        println!("QR code written to {png}");
    }
    if !invite::reach_path(&home).exists() {
        println!("note: `borg mesh serve` has not run here yet; it must be running when they join");
    } else {
        println!("keep `borg mesh serve` running here until they have joined; `borg mesh invites` lists open tickets");
    }
    println!("anyone holding this ticket can join as {role} until it is used or expires: send it privately");
    Ok(())
}

/// `join <ticket>`: the collective goes into collectives.json first (dry run unless `--apply`),
/// with the joiner's `paths` or else a placeholder path, so no repo is routed into it without the
/// joiner choosing one.
async fn join_ticket(place: &Place, text: &str, apply: bool, paths: &[String]) -> Result<(), String> {
    let t = Ticket::decode(text)?;
    let name = t.invite.collective.clone();
    if t.invite.expires_ms <= borg::learn::now_ms() {
        return Err(format!("this ticket expired at {}: ask the admin for a new one", borg::install::utc(t.invite.expires_ms)));
    }
    println!("ticket: join {name:?} as {} (admin {}, expires {})", t.invite.role, short(&t.invite.admin), borg::install::utc(t.invite.expires_ms));
    if !join_config::write(&place.config, &name, &t.relay, paths, apply)? {
        return Ok(());
    }
    let jp = place.named(&name);
    let node = Node::joining(&jp, "0.0.0.0:0".parse().unwrap()).await?.quiet();
    node.online(Duration::from_secs(10)).await;
    let result = tokio::time::timeout(Duration::from_secs(60), node.join(&t)).await;
    node.close().await;
    let (m, rep) = result.map_err(|_| "the admin's node did not answer within 60 s: is `borg mesh serve` running there?".to_string())??;
    let me = node.public();
    println!("joined {name} v{} as {}; first sync: {}", m.version, m.role_of(&me).unwrap_or("?"), show(&rep));
    if paths.is_empty() {
        println!(
            "next: replace {} in {} with the repos that belong to {name} (or join again with --paths DIR...),",
            invite::placeholder(&name),
            place.config.display()
        );
    }
    println!("next: keep `borg mesh serve --collective {name}` running (or `borg mesh serve` inside one of those repos)");
    Ok(())
}

/// The collective's name and store; identity commands work for any collective, never for off.
fn store(r: &Route) -> Result<(String, PathBuf), String> {
    match r {
        Route::Collective { name, home, .. } => Ok((name.clone(), home.clone())),
        Route::Off => Err("this directory belongs to no collective (off)".into()),
    }
}

async fn run(args: &[String]) -> Result<(), String> {
    let place = place(args);
    let r = place.route();
    match args.first().map(String::as_str) {
        Some("id") => println!("{}", Identity::load_or_create(&store(&r)?.1)?.public()),
        Some("init") => {
            let (name, home) = place.gate()?;
            if Membership::load(&home)?.is_some() {
                return Err(format!("{name} already has a membership: {}", membership::path(&home).display()));
            }
            Membership::found(&name, &Identity::load_or_create(&home)?).save(&home)?;
            println!("founded {name}; you are its admin: {}", membership::path(&home).display());
        }
        Some("invite") if positional(args).is_none() => invite_ticket(&place, args)?,
        Some("invites") => {
            let (name, home) = store(&r)?;
            if let Some(id) = flag(args, "--revoke") {
                println!("invite {} withdrawn: its ticket is refused from now on", invite::withdraw(&home, id)?);
                return Ok(());
            }
            let open = invite::pending(&home, borg::learn::now_ms());
            if open.is_empty() {
                println!("no open invites for {name}");
            }
            for (id, p) in open {
                println!("{} {} {} use(s) left, expires {}", &id[..16.min(id.len())], p.role, p.uses_left, borg::install::utc(p.expires_ms));
            }
        }
        Some("join") if positional(args).is_some_and(|a| a.trim().to_ascii_uppercase().starts_with(invite::PREFIX)) => {
            join_ticket(&place, positional(args).unwrap(), args.iter().any(|a| a == "--apply"), &join_config::paths(args)).await?
        }
        Some("invite") => {
            let key = args.get(1).ok_or("invite <key>")?;
            let (name, home) = store(&r)?;
            let mut m = Membership::load(&home)?.ok_or(format!("{name} has no membership: `borg mesh init`"))?;
            m.invite(&Identity::load_or_create(&home)?, key, flag(args, "--role").unwrap_or("member"))?;
            m.save(&home)?;
            // A demoted steerer's directives stop holding here now, not at the next daemon round.
            borg_mesh::steer::enforce(&home, &m, borg::learn::now_ms());
            println!("{key} is a member of {name} (version {}); hand them {} (`borg mesh join` it)", m.version, membership::history_path(&home).display());
        }
        Some("join") => {
            let file = args.get(1).ok_or("join <membership.json|membership-history.jsonl>")?;
            let (name, home) = store(&r)?;
            let docs = membership::parse_docs(&std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?).map_err(|e| format!("{file}: {e}"))?;
            let (doc, moved) = origin::join(&home, &name, &docs).map_err(|e| format!("{file}: {e}"))?;
            borg_mesh::steer::enforce(&home, &doc, borg::learn::now_ms());
            let me = Identity::load_or_create(&home)?.public();
            println!("joined {name} v{} as {}", doc.version, doc.role_of(&me).unwrap_or("NOT LISTED: ask the admin to invite your `borg mesh id`"));
            if moved > 0 {
                println!("{moved} line(s) from revoked members quarantined");
            }
        }
        Some("revoke") => {
            let key = args.get(1).ok_or("revoke <key>")?;
            let (name, home) = store(&r)?;
            if Membership::load(&home)?.is_none() {
                return Err(format!("{name} has no membership"));
            }
            let (m, moved) = origin::revoke(&home, &Identity::load_or_create(&home)?, key, borg::learn::now_ms())?;
            // The revoked signer's pause stops holding here now, not at the next daemon round.
            borg_mesh::steer::enforce(&home, &m, borg::learn::now_ms());
            println!("{key} is revoked from {name} (version {}); every member that syncs learns it", m.version);
            if moved > 0 {
                println!("{moved} line(s) it stamped since quarantined here");
            }
        }
        Some("members") => {
            let (name, home) = store(&r)?;
            let m = Membership::load(&home)?.ok_or(format!("{name} has no membership"))?;
            println!("{name} v{} (signed by {})", m.version, m.admin);
            for x in &m.members {
                println!("{} {}", x.key, x.role);
            }
            for x in &m.revoked {
                println!("{} revoked at {}", x.key, x.at_ms);
            }
        }
        Some("peer") => {
            let (_, home) = store(&r)?;
            match args.get(1).filter(|a| !a.starts_with("--")) {
                Some(target) => {
                    let t = borg_mesh::addr::Target::parse(target)?;
                    peers::remember(&home, &t.key, &borg_mesh::addr::join_hints(&t.hints))?;
                    println!("{t}");
                }
                None => {
                    for t in peers::targets(&home) {
                        println!("{t}");
                    }
                }
            }
        }
        Some("serve") => {
            let bind = flag(args, "--bind").unwrap_or("0.0.0.0:7878").parse().map_err(|e| format!("--bind: {e}"))?;
            let interval = flag(args, "--interval").map(|s| s.parse::<u64>().map_err(|e| format!("--interval: {e}"))).transpose()?.unwrap_or(60);
            let node = Arc::new(Node::bind(&place, bind).await?);
            let relay = node.relay().clone();
            let online = node.online(Duration::from_secs(10)).await;
            let hints = node.hints();
            if hints.is_empty() && relay == Relay::Disabled {
                node.close().await;
                return Err(format!("bound to {bind}, but no interface address another machine could dial was found: pass --bind IP:PORT"));
            }
            invite::save_reach(node.home(), &hints)?;
            let how = match &relay {
                Relay::Default => "n0 public relays + address lookup",
                Relay::Custom(_) => "self-hosted relay",
                Relay::Disabled => "direct addresses only",
            };
            println!("serving {} as {} ({how}; Ctrl-C stops)", store(&r)?.0, node.public());
            println!("reachable at {}", borg_mesh::addr::join_hints(&hints).replace(',', " "));
            if !online {
                eprintln!("borg mesh: no relay connection yet ({}): members behind another NAT may not reach this node until it has one", relay.setting());
            }
            let opts = Options { interval: Duration::from_secs(interval.max(1)), ..Options::default() };
            // The node's goal work for its interactive drones (borg hive/node.rs, phase g G4) on a thread
            // of its own, so no merge or board read holds up a sync round; stopped with the daemon, it
            // writes nothing on the way out (no leave of any goal).
            let quit = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker = {
                let (home, quit) = (node.home().to_path_buf(), quit.clone());
                std::thread::spawn(move || borg::hive::node::serve(&home, &quit))
            };
            daemon::serve(node, opts, stop(), |e| match e {
                Event::Served(Ok(r)) if r.joined > 0 => println!("a new member joined with an invite (`borg mesh members`)"),
                Event::Served(Ok(r)) => println!("served: {}", show(&r)),
                Event::Served(Err(e)) => eprintln!("borg mesh: turned away: {e}"),
                Event::Synced { peer, result: Ok(r) } => println!("synced {}: {}", short(&peer), show(&r)),
                Event::Synced { peer, result: Err(e) } => eprintln!("borg mesh: {}: {e} (backing off)", short(&peer)),
                Event::Gate(Ok(())) => println!("gate open: syncing"),
                Event::Gate(Err(e)) => eprintln!("borg mesh: {e}; paused until the config allows it"),
            })
            .await;
            quit.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
            println!("stopped");
        }
        Some("sync") => {
            let target = args.get(1).ok_or("sync <key>[@<ip:port>]")?;
            let node = Node::bind(&place, "0.0.0.0:0".parse().unwrap()).await?.quiet();
            node.online(Duration::from_secs(10)).await;
            let rep = node.sync(target).await;
            node.close().await;
            let rep = rep?;
            println!("synced: {}", show(&rep));
        }
        Some("fetch") => {
            let (_, home) = place.gate()?;
            let repo = borg::notes::toplevel(&place.dir).ok_or(format!("{} is not in a git repo", place.dir))?;
            let named: Vec<&str> = args[1..].iter().take_while(|a| !a.starts_with("--")).map(String::as_str).collect();
            let commits = borg::notes::wanted(&repo, &named)?;
            if commits.is_empty() {
                println!("every recent commit already has a note");
                return Ok(());
            }
            let m = Membership::load(&home)?.ok_or("no membership")?;
            let node = Node::bind(&place, "0.0.0.0:0".parse().unwrap()).await?.quiet();
            let me = node.public();
            node.online(Duration::from_secs(10)).await;
            for t in peers::targets(&home).into_iter().filter(|t| t.key != me && m.role_of(&t.key).is_some()) {
                match tokio::time::timeout(Duration::from_secs(30), node.fetch(&t.to_string(), &commits)).await {
                    Ok(Ok(rep)) => println!("asked {}: {} excerpt(s) received", short(&t.key), rep.received),
                    Ok(Err(e)) => eprintln!("borg mesh: {}: {e}", short(&t.key)),
                    Err(_) => eprintln!("borg mesh: {}: no answer within 30s", short(&t.key)),
                }
            }
            node.close().await;
            let written = borg::notes::restore(&repo, &home, &commits)?;
            println!("{} note(s) written to {} (not pushed)", written.len(), borg::notes::NOTES_REF);
        }
        Some("steer") => println!("{}", steer_cmd(&place, &r, &args[1..])?),
        _ => return Err(format!("unknown command\n\n{}", help())),
    }
    Ok(())
}

/// `steer <drone|session|all> "<text>" | --pause [text] | --resume [text] | --veto <reason>` (`borg no`
/// for another node's drone runs the last): works for a local
/// collective too (signed with this machine's key, never leaves it); never for an off repo.
fn steer_cmd(place: &Place, r: &Route, args: &[String]) -> Result<String, String> {
    let (name, home) = store(r)?;
    let mut words = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => i += 1,
            "--pause" | "--resume" | "--veto" => {}
            w => words.push(w.to_string()),
        }
        i += 1;
    }
    let kind = if args.iter().any(|a| a == "--pause") {
        borg::steer::PAUSE
    } else if args.iter().any(|a| a == "--resume") {
        borg::steer::RESUME
    } else if args.iter().any(|a| a == "--veto") {
        borg::steer::VETO
    } else {
        borg::steer::SAY
    };
    let usage = "usage: borg steer <drone|session|all> \"<text>\" | <drone> --pause | <drone> --resume | <drone> --veto \"<reason>\" [--dir DIR]";
    let target = words.first().ok_or(usage)?;
    let text = words[1..].join(" ");
    if (kind == borg::steer::SAY || kind == borg::steer::VETO) && text.trim().is_empty() {
        return Err(usage.into());
    }
    let engineer = borg::learn::engineer(&place.dir);
    borg_mesh::steer::issue(&home, &name, borg::collective::may_sync(r), &engineer, target, kind, &text, borg::learn::now_ms())
}

/// Ctrl-C or SIGTERM.
async fn stop() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term => {}
    }
}

/// `borg mesh --help`: the commands grouped by who runs them.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Start a team (admin)",
        &[
            ("init", "found the collective; you become its admin"),
            ("invite [--role R] [--expires 7d] [--uses 1] [--png FILE]", "a ticket (and QR code) for a new member"),
            ("invite KEY [--role R]", "add a member by key (the manual flow)"),
            ("invites [--revoke ID]", "open tickets, or withdraw one"),
            ("revoke KEY", "remove a member"),
        ],
    ),
    (
        "Join and run",
        &[
            ("join TICKET [--paths DIR...] [--apply]", "join with a ticket; DIR: your repos for it (a dry run without --apply)"),
            ("join FILE", "join with a membership history file (the manual flow)"),
            ("serve [--bind IP:PORT] [--interval SECS]", "keep syncing; leave it running"),
            ("sync KEY[@HINTS]", "sync once with one member"),
            ("peer [KEY[@HINTS]]", "the peers this node dials, or add one"),
        ],
    ),
    (
        "Look up",
        &[("id", "this node's public key in the collective"), ("members", "the signed member list"), ("fetch [COMMIT...]", "reasoning notes from members")],
    ),
];

fn help() -> String {
    let width = HELP.iter().flat_map(|(_, rows)| rows.iter().map(|(c, _)| c.chars().count())).max().unwrap_or(0);
    let mut out = String::from("Usage: borg mesh <command> [--dir DIR | --collective NAME]\n");
    for (group, rows) in HELP {
        out.push_str(&format!("\n{group}\n"));
        for (cmd, what) in *rows {
            out.push_str(&format!("  {cmd:<width$}  {what}\n"));
        }
    }
    out.push_str("\nThe collective is the one the current directory belongs to, unless --dir or --collective names another.\n");
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), None | Some("help" | "-h" | "--help")) {
        print!("{}", help());
        return;
    }
    // The daemon's node worker reads its drones' scratch state where their hooks and `borg hive` keep it
    // (borg hook.rs `main`, hive/mod.rs `Here::locate`): the store's `state` unless BORG_STATE names one.
    if args.first().map(String::as_str) == Some("serve")
        && std::env::var_os("BORG_STATE").is_none()
        && let Route::Collective { home, .. } = place(&args).route()
    {
        // SAFETY: single-threaded: the runtime and its threads start below.
        unsafe { std::env::set_var("BORG_STATE", home.join("state")) };
    }
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("borg mesh: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(run(&args)) {
        eprintln!("borg mesh: {e}");
        std::process::exit(1);
    }
}
