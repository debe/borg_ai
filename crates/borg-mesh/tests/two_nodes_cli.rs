//! Two machines of one collective converse through the mesh without a network: a Claude Code session on
//! node A and an opencode session on node B, each driven through the real `borg hook` and `borg tell`,
//! and between them a sync session simulated over the same functions a session runs (sync.rs): each
//! side stamps its lines and offers what it holds (`offered`), both reconcile in memory (recon.rs) and
//! hand each other their `Lines` frames (`frame`), as JSON as the wire carries them, which the other
//! merges (`Store::store_lines`), and presence goes both ways (live.rs). The network is what this
//! replaces: iroh itself is not under test here.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use borg::msg::{Author, Line, Pace};
use borg_mesh::identity::Identity;
use borg_mesh::lines;
use borg_mesh::live;
use borg_mesh::membership::Membership;
use borg_mesh::origin::{self, Origin};
use borg_mesh::recon::Recon;
use borg_mesh::sync::{self, Report, Signed, Store};
use serde_json::{Value, json};

/// The collective both machines are members of.
const COLLECTIVE: &str = "team";

/// The `borg` binary of this workspace, built once per test process: a dependency's binary is not
/// built for this crate's tests, and one left in the target directory may be stale.
fn borg_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        // Beside this crate's own binary: the same target directory and profile.
        let mesh = Path::new(env!("CARGO_BIN_EXE_borg-mesh"));
        let profile_dir = mesh.parent().unwrap();
        let target = profile_dir.parent().unwrap();
        let profile = match profile_dir.file_name().unwrap().to_str().unwrap() {
            "debug" => "dev",
            p => p,
        };
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let out = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "borg-ai", "--bin", "borg", "--profile", profile, "--manifest-path"])
            .arg(&manifest)
            .arg("--target-dir")
            .arg(target)
            .output()
            .unwrap();
        assert!(out.status.success(), "building borg: {}", String::from_utf8_lossy(&out.stderr));
        mesh.with_file_name("borg")
    })
}

/// One machine: its root (config, `BORG_HOME`, a repo at work/app), its node key and the lock its
/// store's writes take, as `Node` holds them.
struct Machine {
    root: PathBuf,
    home: PathBuf,
    me: Identity,
    lock: Mutex<()>,
}

impl Machine {
    fn new(name: &str) -> Machine {
        let root = std::env::temp_dir().join(format!("borg-mesh-two-nodes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work/app");
        std::fs::create_dir_all(&work).unwrap();
        let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&work).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
        git(&["init", "-q"]);
        git(&["config", "user.email", format!("{name}@x").as_str()]);
        let cfg = json!({"default": "off", "collectives": [{"name": COLLECTIVE, "paths": [root.join("work")], "share": "team", "relay": "disabled"}]});
        std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
        let home = root.join("base/collectives").join(COLLECTIVE);
        // Also publishes the key as this node's name in messages (msg.rs `node`).
        let me = Identity::load_or_create(&home).unwrap();
        Machine { root, home, me, lock: Mutex::new(()) }
    }

    fn key(&self) -> String {
        self.me.public()
    }

    fn work(&self) -> PathBuf {
        self.root.join("work/app")
    }

    /// The store as a sync session merges into it.
    fn store(&self) -> Store<'_> {
        Store { home: &self.home, lock: &self.lock, me: &self.me }
    }

    fn log(&self) -> PathBuf {
        self.home.join(borg::msg::FILE)
    }

    /// `borg args…` on this machine, `stdin` its input, with only `env` on top of a bare environment:
    /// (exit code, stdout, stderr).
    fn borg(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> (Option<i32>, String, String) {
        let mut child = Command::new(borg_bin())
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("BORG_MATCH", "bm25")
            .env("BORG_HOME", self.root.join("base"))
            .env("BORG_CONFIG", self.root.join("collectives.json"))
            .env("BORG_LEARN", "off")
            // No release check from a test: it would ask GitHub (borg update.rs).
            .env("BORG_UPDATE_CHECK", "off")
            .env("BORG_QUEEN", "off")
            .env("BORG_VECTORS", "off")
            .env("BORG_MODE", "hook")
            .envs(env.iter().copied())
            .current_dir(self.work())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
    }

    /// The hook of `harness` for `event` with `input`, as the harness calls it: its answer (Null: none).
    fn hook(&self, harness: &str, event: &str, input: &Value) -> Value {
        let (code, out, err) = self.borg(&["hook", harness, event], &[], &input.to_string());
        assert_eq!(code, Some(0), "{harness} {event}: {err}");
        if out.trim().is_empty() { Value::Null } else { serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("not JSON: {out}")) }
    }

    /// Claude Code's recorded prompt payload (borg's tests/fixtures/claude) for `session`.
    fn claude_prompt(&self, session: &str) -> Value {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../borg/tests/fixtures/claude/prompt.json");
        let text = std::fs::read_to_string(p).unwrap();
        serde_json::from_str(&text.replace("{{session}}", session).replace("{{cwd}}", &self.work().display().to_string()).replace("{{transcript}}", ""))
            .unwrap()
    }

    /// The canonical prompt input the opencode adapter sends for `session`.
    fn opencode_prompt(&self, session: &str) -> Value {
        json!({"session_id": session, "cwd": self.work().display().to_string(), "prompt": "carry on"})
    }

    /// `borg tell args…` with `env`: the id from "sent <id> to …", or the refusal on stderr.
    fn tell(&self, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
        let a: Vec<&str> = std::iter::once("tell").chain(args.iter().copied()).collect();
        match self.borg(&a, env, "") {
            (Some(0), out, _) => {
                let mut w = out.split_whitespace();
                assert_eq!(w.next(), Some("sent"), "{out}");
                Ok(w.next().unwrap().to_string())
            }
            (_, _, err) => Err(err),
        }
    }

    fn lines(&self) -> Vec<Line> {
        borg::msg::lines(&self.home)
    }
}

/// The additional context a hook answer carries (Claude-shaped, for every harness).
fn context(v: &Value) -> String {
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

/// The lines as the wire carries them: JSON out, JSON in.
fn wire(lines: Vec<Signed>) -> Vec<Signed> {
    serde_json::from_str(&serde_json::to_string(&lines).unwrap()).unwrap()
}

/// One sync session between `a` and `b` over the messages and presence, with no network: what each
/// side's `Node::exchange` does for them once the handshake and the membership step are done (each
/// holds the same membership here). Both stamp their lines, reconcile the sets they offer round by
/// round in lock step, hand over the frames and merge what they got. Returns what `a` and `b` stored.
fn session(a: &Machine, b: &Machine) -> (Report, Report) {
    let held = |m: &Machine| Membership::load(&m.home).unwrap().unwrap();
    let (ma, mb) = (held(a), held(b));
    a.store().stamp().unwrap();
    b.store().stamp().unwrap();
    let (oa, ob) = (origin::load(&a.home), origin::load(&b.home));
    let now = borg::learn::now_ms();
    let (sa, sb) = (sync::offered(&a.log(), &oa, &ma, now, false), sync::offered(&b.log(), &ob, &mb, now, false));
    let (mut ra, mut rb) = (Recon::new(&sa), Recon::new(&sb));
    loop {
        let (x, y) = (ra.message(), rb.message());
        let done = ra.absorb(&x, y.clone()).unwrap();
        assert_eq!(rb.absorb(&y, x).unwrap(), done, "both sides close in the same round");
        if done {
            break;
        }
    }
    let (to_b, to_a) = (wire(sync::frame(&sa, &oa, ra.give.iter())), wire(sync::frame(&sb, &ob, rb.give.iter())));
    let (mut at_a, mut at_b) = (Report::default(), Report::default());
    a.store().store_lines(&a.log(), to_a, &ra.want, &ma, None, &mut at_a).unwrap();
    b.store().store_lines(&b.log(), to_b, &rb.want, &mb, None, &mut at_b).unwrap();
    // Live: presence both ways, received under the store's lock as a session does.
    let (pa, pb) = (live::outgoing(&a.home, &a.me, now), live::outgoing(&b.home, &b.me, now));
    {
        let _w = a.lock.lock().unwrap();
        at_a.presence += live::receive(&a.home, &ma, &a.key(), pb, now).unwrap();
    }
    {
        let _w = b.lock.lock().unwrap();
        at_b.presence += live::receive(&b.home, &mb, &b.key(), pa, now).unwrap();
    }
    (at_a, at_b)
}

/// What `received`, `rejected` and `presence` of a report say.
fn moved(r: &Report) -> (usize, usize, usize) {
    (r.received, r.rejected, r.presence)
}

/// A Claude Code session on A tells an opencode session on B; B's next prompt shows it and acknowledges
/// it; B answers; A's next prompt shows the answer, and A's window to B's session has opened: before the
/// acknowledgement reached A a second tell is refused, after it the same tell goes. A non-member's line,
/// and a seen line A signs for B's session, are refused by the merge.
#[test]
fn a_claude_session_and_an_opencode_session_converse_across_two_nodes() {
    let (a, b) = (Machine::new("a"), Machine::new("b"));
    let mut m = Membership::found(COLLECTIVE, &a.me);
    m.invite(&a.me, &b.key(), "member").unwrap();
    m.save(&a.home).unwrap();
    m.save(&b.home).unwrap();
    let claude = [("CLAUDE_CODE_SESSION_ID", "s-claude")];
    let to_b = format!("s-oc@{}", b.key());

    // Both sessions start; until a session carries B's presence, A knows no drone of B's node.
    a.hook("claude", "prompt", &a.claude_prompt("s-claude"));
    b.hook("opencode", "prompt", &b.opencode_prompt("s-oc"));
    let unknown = a.tell(&[&to_b, "hello"], &claude).unwrap_err();
    assert!(unknown.contains("no drone or message here names node"), "{unknown}");
    let (at_a, at_b) = session(&a, &b);
    assert_eq!((moved(&at_a), moved(&at_b)), ((0, 0, 1), (0, 0, 1)), "presence only: no message is held yet");

    // A's session tells B's; a second tell is refused: B's node has acknowledged nothing of A's yet.
    let one = a.tell(&[&to_b, "the", "schema", "is", "frozen"], &claude).unwrap();
    let refused = a.tell(&[&to_b, "and", "the", "API"], &claude).unwrap_err();
    assert!(refused.contains("has acknowledged no message yet"), "{refused}");
    let window = |at: &Machine| borg::msg::window_for(&at.lines(), Author { node: &a.key(), by: "s-claude" }, &to_b, borg::learn::now_ms());
    assert_eq!(window(&a).pace, Pace::Rate);

    // Sync: the tell reaches B, signed by A.
    let (at_a, at_b) = session(&a, &b);
    assert_eq!((at_a.received, at_b.received, at_b.rejected), (0, 1, 0), "the tell went to B, nothing came back");
    let line = std::fs::read_to_string(b.log()).unwrap().lines().find(|l| l.contains(&one)).map(lines::hash).unwrap();
    assert_eq!(origin::load(&b.home).get(&line).map(|o| o.key.clone()), Some(a.key()), "with A's signed origin");

    // B's next prompt shows it, names it for the adapter, and acknowledges it.
    let shown = b.hook("opencode", "prompt", &b.opencode_prompt("s-oc"));
    assert!(context(&shown).contains(&format!("[{one}]: the schema is frozen")) && context(&shown).contains("(claude, node"), "{shown}");
    assert_eq!(shown["shown_ids"], json!([one]), "{shown}");
    let seen_by_b = |at: &Machine| at.lines().iter().any(|l| matches!(l, Line::Seen(s) if s.node == b.key() && s.by == "s-oc" && s.ids.contains(&one)));
    assert!(seen_by_b(&b), "B's delivery wrote its seen line");
    assert!(!context(&b.hook("opencode", "prompt", &b.opencode_prompt("s-oc"))).contains(&one), "shown once");

    // B answers, threaded on the tell. Until a session carries B's acknowledgements, A's window is shut.
    let to_a = format!("s-claude@{}", a.key());
    let reply = b.tell(&[&to_a, "--re", &one, "--from", "s-oc", "noted,", "holding", "the", "migration"], &[]).unwrap();
    assert!(a.tell(&[&to_b, "and", "the", "API"], &claude).is_err(), "B's acknowledgements are on B only");

    // Sync: the seen line and the reply reach A.
    let (at_a, at_b) = session(&a, &b);
    assert_eq!((at_a.received, at_a.rejected, at_b.received), (2, 0, 0), "B's seen line and its reply");
    assert!(seen_by_b(&a), "the seen line is A's now");

    // A's next prompt shows the reply.
    let answer = a.hook("claude", "prompt", &a.claude_prompt("s-claude"));
    let c = context(&answer);
    assert!(c.contains(&format!("[{reply}] (re {one}): noted, holding the migration")) && c.contains("(opencode, node"), "{c}");
    assert!(answer.get("shown_ids").is_none(), "Claude Code gets only the keys it defines: {answer}");

    // A's window to B's session: paced by acknowledgements now, and one acknowledged doubled it.
    let w = window(&a);
    assert_eq!((w.pace, w.window, w.outstanding), (Pace::Window, 2, 0), "{w:?}");
    let two = a.tell(&[&to_b, "and", "the", "API"], &claude).expect("the window opened");
    let (_, at_b) = session(&a, &b);
    assert_eq!((at_b.received, at_b.rejected), (2, 0), "A's seen line for the reply, and the second tell");
    assert!(context(&b.hook("opencode", "prompt", &b.opencode_prompt("s-oc"))).contains(&format!("[{two}]: and the API")));

    // Forgeries the merge refuses. A non-member's tell, signed by it and naming its own node.
    let outsider = Identity::load_or_create(&a.root.join("outsider")).unwrap();
    let from = borg::msg::Sender { by: "s-x".into(), name: "s-x".into(), harness: "claude".into(), node: outsider.public() };
    let now = borg::learn::now_ms();
    let forged = serde_json::to_string(&borg::msg::Message::new(borg::msg::TELL, &from, &to_b, None, now, "drop the table")).unwrap();
    let rejected = |at: &Machine, signer: &Identity, line: &str| {
        let h = lines::hash(line);
        let mut r = Report::default();
        let got = wire(vec![Signed { origin: Origin::new(signer, &h, now), line: line.to_string() }]);
        at.store().store_lines(&at.log(), got, &[h].into(), &Membership::load(&at.home).unwrap().unwrap(), None, &mut r).unwrap();
        assert_eq!((r.received, r.rejected), (0, 1), "{line}");
        assert!(!std::fs::read_to_string(at.log()).unwrap().contains(line), "{line}");
    };
    rejected(&b, &outsider, &forged);
    // A seen line A signs, claiming B's session saw what it did not: it would open A's window to it.
    let three = a.tell(&[&to_b, "one", "more"], &claude).unwrap();
    let claim = serde_json::to_string(&borg::msg::Seen::new("s-oc", &b.key(), now, &[&three])).unwrap();
    rejected(&b, &a.me, &claim);
    rejected(&a, &a.me, &claim);
    // Nor does A offer it, held and stamped as its own.
    std::fs::OpenOptions::new().append(true).open(a.log()).unwrap().write_all(format!("{claim}\n").as_bytes()).unwrap();
    a.store().stamp().unwrap();
    let offer = sync::offered(&a.log(), &origin::load(&a.home), &Membership::load(&a.home).unwrap().unwrap(), now, false);
    assert!(!offer.contains_key(&lines::hash(&claim)) && offer.values().any(|l| l.contains(&three)), "A offers its tell, not the claim");

    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}
