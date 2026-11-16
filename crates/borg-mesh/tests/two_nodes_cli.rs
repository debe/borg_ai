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
use borg_mesh::cache::Cache;
use borg_mesh::identity::Identity;
use borg_mesh::lines;
use borg_mesh::live;
use borg_mesh::membership::Membership;
use borg_mesh::origin::{self, Index, Origin};
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

/// One machine: its root (config, `BORG_HOME`, a repo at work/app), its node key, the lock its
/// store's writes take and what its sessions read of it, as `Node` holds them.
struct Machine {
    root: PathBuf,
    home: PathBuf,
    me: Identity,
    lock: Mutex<()>,
    cache: Cache,
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
        Machine { root, home, me, lock: Mutex::new(()), cache: Cache::default() }
    }

    fn key(&self) -> String {
        self.me.public()
    }

    fn work(&self) -> PathBuf {
        self.root.join("work/app")
    }

    /// The store as a sync session merges into it.
    fn store(&self) -> Store<'_> {
        Store { home: &self.home, lock: &self.lock, me: &self.me, cache: &self.cache }
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
    let (oa, ob) = (a.cache.origins(&a.home), b.cache.origins(&b.home));
    let now = borg::learn::now_ms();
    let offer = |m: &Machine, o: &Index, held: &Membership| sync::offered(&m.log(), m.cache.log(&m.log()), o, held, now, false);
    let (sa, sb) = (offer(a, &oa, &ma), offer(b, &ob, &mb));
    let (mut ra, mut rb) = (Recon::new(sa.set()), Recon::new(sb.set()));
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
    // Live: presence both ways, after each records the clones in its collective's paths, received under
    // the store's lock as a session does.
    for m in [a, b] {
        borg::handoff::discover(&m.home, &m.root.join("collectives.json"));
    }
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
    assert!(refused.contains("has acknowledged no message from this machine yet"), "{refused}");
    let window = |at: &Machine| {
        borg::msg::window_for(
            &at.lines().iter().filter_map(Line::borrowed).collect::<Vec<_>>(),
            Author { node: &a.key(), by: "s-claude" },
            &to_b,
            borg::learn::now_ms(),
        )
    };
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
    assert_eq!((w.pace, w.window, w.outstanding), (Pace::Floor, 2, 0), "{w:?}");
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
    let offer = sync::offered(&a.log(), a.cache.log(&a.log()), &a.cache.origins(&a.home), &Membership::load(&a.home).unwrap().unwrap(), now, false);
    assert!(!offer.contains_key(&lines::hash(&claim)) && offer.iter().any(|(_, l)| l.contains(&three)), "A offers its tell, not the claim");

    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}

/// `git args…` in `dir`, with no global or system config (a test's own identity and branch).
fn git(dir: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Two machines and a bare remote both clone: A's clone has history of A's (author, time) and work in
/// progress only A wrote; B's has a commit of its own on another branch, with another author and time.
/// Both are members, and both sessions have started (so each machine's clone list holds the clone).
struct Pair {
    a: Machine,
    b: Machine,
}

impl Pair {
    fn new(tag: &str) -> Pair {
        let (a, b) = (Machine::new(&format!("{tag}-a")), Machine::new(&format!("{tag}-b")));
        let remote = a.root.join("remote.git");
        let gh = a.root.join("home");
        std::fs::create_dir_all(&gh).unwrap();
        git(&a.root, &gh, &["init", "-q", "--bare", "-b", "main", &remote.to_string_lossy()], &[]);
        let (wa, wb) = (a.work(), b.work());
        git(&wa, &gh, &["checkout", "-q", "-b", "main"], &[]);
        git(&wa, &gh, &["config", "user.name", "a"], &[]);
        std::fs::write(wa.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&wa, &gh, &["add", "-A"], &[]);
        let a_time = [("GIT_AUTHOR_DATE", "2026-01-02T03:04:05Z"), ("GIT_COMMITTER_DATE", "2026-01-02T03:04:05Z")];
        git(&wa, &gh, &["commit", "-q", "-m", "a starts"], &a_time);
        git(&wa, &gh, &["remote", "add", "origin", &remote.to_string_lossy()], &[]);
        git(&wa, &gh, &["push", "-q", "-u", "origin", "main"], &[]);
        std::fs::remove_dir_all(&wb).unwrap();
        git(&b.root, &gh, &["clone", "-q", &remote.to_string_lossy(), &wb.to_string_lossy()], &[]);
        git(&wb, &gh, &["config", "user.email", &format!("{tag}-b@x")], &[]);
        git(&wb, &gh, &["config", "user.name", "b"], &[]);
        git(&wb, &gh, &["checkout", "-q", "-b", "b-own"], &[]);
        std::fs::write(wb.join("b.rs"), "fn b() {}\n").unwrap();
        git(&wb, &gh, &["add", "-A"], &[]);
        let b_time = [("GIT_AUTHOR_DATE", "2026-03-04T05:06:07Z"), ("GIT_COMMITTER_DATE", "2026-03-04T05:06:07Z")];
        git(&wb, &gh, &["commit", "-q", "-m", "b's own"], &b_time);
        git(&wb, &gh, &["checkout", "-q", "main"], &[]);
        // A's work in progress, which only A wrote.
        std::fs::write(wa.join("lib.rs"), "fn a() { b() }\n").unwrap();
        std::fs::write(wa.join("a_only.rs"), "// written on A alone\nfn only_a() {}\n").unwrap();
        let mut m = Membership::found(COLLECTIVE, &a.me);
        m.invite(&a.me, &b.key(), "member").unwrap();
        m.save(&a.home).unwrap();
        m.save(&b.home).unwrap();
        a.hook("claude", "prompt", &a.claude_prompt("s-a"));
        b.hook("claude", "prompt", &b.claude_prompt("s-b"));
        Pair { a, b }
    }

    fn gh(&self) -> PathBuf {
        self.a.root.join("home")
    }

    /// `borg handoff …` on `m` as the Claude Code session `session`.
    fn handoff(&self, m: &Machine, session: &str, args: &[&str]) -> (Option<i32>, String, String) {
        let a: Vec<&str> = std::iter::once("handoff").chain(args.iter().copied()).collect();
        m.borg(&a, &[("CLAUDE_CODE_SESSION_ID", session)], "")
    }

    /// Offers A's work as `s-a`, asking `to`: the hand-off id, or the refusal.
    fn offer(&self, to: Option<&str>) -> Result<String, String> {
        let brief = self.a.root.join("brief.md");
        std::fs::write(&brief, "# Lib calls b\n\n## Next step\ntests for b\n").unwrap();
        let mut args = vec!["lib calls b; next: tests", "--brief", brief.to_str().unwrap()];
        if let Some(t) = to {
            args.extend(["--to", t]);
        }
        match self.handoff(&self.a, "s-a", &args) {
            (Some(0), out, _) => {
                let at = out.find("hand-off h").unwrap() + "hand-off ".len();
                Ok(out[at..at + 13].to_string())
            }
            (_, out, err) => Err(format!("{out}{err}")),
        }
    }

    /// Whether A's hook refuses `tool` with `input` for its session.
    fn refused_on_a(&self, tool: &str, input: Value) -> bool {
        let ev = json!({"session_id": "s-a", "cwd": self.a.work(), "tool_name": tool, "tool_input": input, "hook_event_name": "PreToolUse"});
        self.a.hook("claude", "pre_tool", &ev)["hookSpecificOutput"]["permissionDecision"] == "deny"
    }

    fn clean(self) {
        let _ = std::fs::remove_dir_all(&self.a.root);
        let _ = std::fs::remove_dir_all(&self.b.root);
    }
}

/// Across two machines, through the mesh simulated as a session does it and a bare remote both clone:
/// an ask to an engineer whose machine is not seen yet is refused with the gap; once presence carried
/// B's machine (its borg, its clone of the repo), A's offer asks B; the hand-off line reaches B, where
/// `borg handoff` lists it as asked of B and B's next prompt is told; B takes it: its tree is A's
/// working tree (a file only A wrote), fetched from the remote (objects B did not have); the taken line
/// reaches A, which is fenced at once: its edits in the repo refused, its reads passing; and told by
/// whom.
#[test]
fn an_offer_on_one_node_is_asked_of_and_taken_on_the_other_and_the_original_is_fenced() {
    let p = Pair::new("take");
    let (a, b) = (&p.a, &p.b);
    let gap = p.offer(Some("take-b@x")).unwrap_err();
    assert!(gap.contains("no machine of take-b@x was seen") && gap.contains("nobody asked"), "{gap}");
    assert_eq!(git(&a.work(), &p.gh(), &["ls-remote", "origin", "refs/borg/*"], &[]), "", "a refused ask pushes nothing");
    let gap = p.offer(Some("nobody@x")).unwrap_err();
    assert!(gap.contains("no machine of nobody@x"), "{gap}");

    // Presence carries B's machine record to A.
    session(a, b);
    let id = p.offer(Some("take-b@x")).expect("the check passes: B speaks hand-off and has a clone");
    let state = git(&a.work(), &p.gh(), &["ls-remote", "origin", &format!("refs/borg/handoff/{id}/state")], &[]);
    let state = state.split_whitespace().next().unwrap().to_string();
    assert!(Command::new("git").arg("-C").arg(b.work()).args(["cat-file", "-e", &state]).status().unwrap().code() != Some(0), "B lacks the state");

    // The mesh carries the offer, which asks B.
    let (_, at_b) = session(a, b);
    assert!(at_b.received >= 1 && at_b.rejected == 0, "{at_b:?}");
    let (code, out, err) = p.handoff(b, "s-b", &[]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains(&id) && out.contains("asked") && out.contains("asks take-b@x (you)"), "{out}");
    let told = b.hook("claude", "prompt", &b.claude_prompt("s-b"));
    let c = context(&told);
    assert!(c.contains(&format!("Hand-off {id} from take-a@x")) && c.contains("(asked of you)") && c.contains("Peer data"), "{told}");
    assert!(!context(&b.hook("claude", "prompt", &b.claude_prompt("s-b"))).contains(&id), "told once");
    assert!(std::fs::read_to_string(b.log()).unwrap().lines().any(|l| l.contains("engineer:take-b@x") && l.contains(&id)), "the ask's message to B");

    // B takes it: the transfer, objects included.
    let (code, out, err) = p.handoff(b, "s-b", &["take", &id]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains(&format!("took hand-off {id} from take-a@x")) && out.contains("tests for b"), "{out}");
    assert_eq!(std::fs::read_to_string(b.work().join("a_only.rs")).unwrap(), "// written on A alone\nfn only_a() {}\n", "A's file, on B");
    assert_eq!(std::fs::read_to_string(b.work().join("lib.rs")).unwrap(), "fn a() { b() }\n");
    assert_eq!(git(&b.work(), &p.gh(), &["rev-parse", "HEAD"], &[]), state, "B is at the state");
    assert!(!b.work().join("b.rs").exists(), "B's own branch is not in it");

    // The taken line reaches A: fenced at once, told who took it.
    let (at_a, _) = session(a, b);
    assert!(at_a.received >= 1 && at_a.rejected == 0, "{at_a:?}");
    assert!(p.refused_on_a("Edit", json!({"file_path": a.work().join("lib.rs")})), "A's edit there is refused");
    assert!(p.refused_on_a("Bash", json!({"command": "echo x >> lib.rs"})));
    assert!(!p.refused_on_a("Read", json!({"file_path": a.work().join("lib.rs")})), "A's reads pass");
    let notice = context(&a.hook("claude", "prompt", &a.claude_prompt("s-a")));
    assert!(notice.contains(&format!("Hand-off {id} was taken by take-b@x")), "{notice}");
    let (_, listed, _) = p.handoff(a, "s-a", &[]);
    assert!(listed.contains("taken ") && listed.contains("by take-b@x") && listed.contains("the remote holds it"), "{listed}");
    p.clean();
}

/// The receiver's borg never recorded its clone (no session opened it since), and its remote names the
/// repo by another spelling: B's mesh records the clone from the collective's paths before its presence
/// leaves; A's ask, finding no clone of that URL among B's, goes out with a warning instead of a
/// refusal; B takes it in that clone, its remote proven by holding the hand-off.
#[test]
fn an_ask_reaches_a_clone_no_session_opened_under_another_spelling() {
    let p = Pair::new("unopened");
    let (a, b) = (&p.a, &p.b);
    std::fs::remove_file(b.home.join("handoff/clones.json")).unwrap();
    std::os::unix::fs::symlink(&a.root, b.root.join("link")).unwrap();
    git(&b.work(), &p.gh(), &["remote", "set-url", "origin", &b.root.join("link/remote.git").to_string_lossy()], &[]);
    session(a, b);
    let recorded = std::fs::read_to_string(b.home.join("handoff/clones.json")).unwrap();
    assert!(recorded.contains("\"name\":\"app\""), "B's mesh recorded the clone: {recorded}");
    let brief = a.root.join("brief.md");
    std::fs::write(&brief, "# Lib calls b\n\n## Next step\ntests for b\n").unwrap();
    let (code, out, err) = p.handoff(a, "s-a", &["lib calls b", "--brief", brief.to_str().unwrap(), "--to", "unopened-b@x"]);
    assert_eq!(code, Some(0), "an ask, not a refusal: {out}{err}");
    assert!(err.contains("asking unopened-b@x although no machine of theirs told the team it holds a clone of"), "{err}");
    let id = out[out.find("hand-off h").unwrap() + "hand-off ".len()..][..13].to_string();
    session(a, b);
    let (code, out, err) = p.handoff(b, "s-b", &["take", &id]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(std::fs::read_to_string(b.work().join("lib.rs")).unwrap(), "fn a() { b() }\n", "A's work, on B");
    p.clean();
}

/// Declined across machines: B says no; the decline reaches A, which lists it with who declined, is told
/// in a message, and keeps the offer: nothing is fenced, and A takes it back. A decline from someone the
/// offer does not ask is refused.
#[test]
fn a_declined_ask_stays_the_owners() {
    let p = Pair::new("decline");
    let (a, b) = (&p.a, &p.b);
    session(a, b);
    let id = p.offer(Some("decline-b@x")).unwrap();
    session(a, b);
    let (code, _, err) = p.handoff(a, "s-a", &["decline", &id]);
    assert!(code == Some(1) && err.contains("not you"), "only whom it asks declines: {err}");
    let (code, out, err) = p.handoff(b, "s-b", &["decline", &id]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("it stays theirs; they are told"), "{out}");
    let (code, _, err) = p.handoff(b, "s-b", &["decline", &id]);
    assert!(code == Some(1) && err.contains("declined it already"), "a second decline: {err}");
    session(a, b);
    let (_, listed, _) = p.handoff(a, "s-a", &[]);
    assert!(listed.contains("declined") && listed.contains("declined by decline-b@x"), "{listed}");
    let told = context(&a.hook("claude", "prompt", &a.claude_prompt("s-a")));
    assert!(told.contains(&format!("Hand-off {id} was declined by decline-b@x")), "{told}");
    assert!(!p.refused_on_a("Edit", json!({"file_path": a.work().join("lib.rs")})), "nothing fenced");
    let (code, out, err) = p.handoff(a, "s-a", &["take", &id]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("took back"), "{out}");
    p.clean();
}
