//! The hive across machines (docs/design/hive.md C4), proven on two loopback mesh nodes in
//! one process with drones run as the real `borg` binary (its hooks and `borg hive` CLI, what a sim
//! drone does): a task opened on A is claimed on both nodes before they sync, one claim wins on both,
//! the work is done on B, verified on A by a drone of the same name, credited once on the author's
//! node, and the boards fold identically on both. A message (`borg hive tell`) reaches the verifier on
//! the other node once. No model call: every model CLI on the drones' PATH is a stub that fails.
//!
//! In the first test both nodes hold the repo: A's checkout is `a/app`, B's a `git worktree` of it at
//! `b/app`. The stores are separate, the git objects shared, which is what "both nodes hold the repo"
//! means for a verdict (it diffs the author's snapshots).
//!
//! In the second each node has a clone of one remote with an object store of its own: B's attempt
//! travels as a bundle (borg hive/commits.rs), A verifies and integrates it, its combined tree holds
//! B's commit; a forged bundle and one over the cap are refused; no secret in a task title, a contract
//! or a message reaches either store.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use borg::hive::{self, Ev, Status, board};
use borg_mesh::identity::Identity;
use borg_mesh::membership::Membership;
use borg_mesh::sync::{Node, Place, Report};
use serde_json::{Value, json};

const LOCAL: &str = "127.0.0.1:0";

/// One machine: a root store (`BORG_HOME`), a config routing its checkout `app` to collective "work"
/// (team, relays off), and a HOME.
struct Machine {
    base: PathBuf,
    app: PathBuf,
}

impl Machine {
    fn new(root: &Path, name: &str) -> Machine {
        let base = root.join(name);
        let app = base.join("app");
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("collectives/work")).unwrap();
        let cfg = json!({"default": "off", "collectives": [{"name": "work", "paths": [app], "share": "team", "relay": "disabled"}]});
        std::fs::write(base.join("collectives.json"), cfg.to_string()).unwrap();
        Machine { base, app }
    }

    fn store(&self) -> PathBuf {
        self.base.join("collectives/work")
    }

    fn place(&self) -> Place {
        Place { config: self.base.join("collectives.json"), base: self.base.clone(), dir: self.app.display().to_string(), collective: None }
    }

    /// The `borg` binary with this machine's environment and `env`, in its checkout. Its hook state is
    /// its own (`BORG_STATE`): tests running at once reuse session ids.
    fn borg(&self, root: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut c = Command::new(borg_bin());
        c.args(args)
            .env_clear()
            .env("BORG_STATE", self.base.join("state"))
            .env("PATH", format!("{}:/usr/local/bin:/usr/bin:/bin", root.join("bin").display()))
            .env("HOME", self.base.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("BORG_HOME", &self.base)
            .env("BORG_CONFIG", self.base.join("collectives.json"))
            .env("BORG_QUEEN", "off")
            .env("BORG_VECTORS", "off")
            .env("BORG_MATCH", "bm25")
            .env("BORG_PREDICT_MODEL", "off")
            .env("BORG_STOP_ATTENTION", "off")
            .env("BORG_MODE", "hook")
            .envs(env.iter().copied())
            .current_dir(&self.app)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    /// `borg hive <args> --dir <app>` as drone `env`: (stdout + stderr, exit code).
    fn hive(&self, root: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, i32) {
        let dir = self.app.display().to_string();
        let all: Vec<&str> = ["hive"].into_iter().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
        let out = self.borg(root, &all, env).output().unwrap();
        (format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)), out.status.code().unwrap_or(-1))
    }

    fn ok(&self, root: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let (out, code) = self.hive(root, args, env);
        assert_eq!(code, 0, "borg hive {args:?}: {out}");
        out
    }

    /// The prompt hook of `session` (a drone of the hive with `env`): its added context.
    fn prompt(&self, root: &Path, session: &str, env: &[(&str, &str)]) -> String {
        use std::io::Write;
        let mut child = self.borg(root, &["hook", "claude", "prompt"], env).spawn().unwrap();
        let input = json!({"session_id": session, "cwd": self.app, "prompt": "carry on with the goal"});
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).to_string();
        serde_json::from_str::<Value>(out.trim()).ok().and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)).unwrap_or_default()
    }

    fn board(&self) -> board::Board {
        board::fold(&board::read(&hive::tasks_path(&self.store(), "g1")))
    }
}

/// The `borg` binary beside this test's `borg-mesh` (scripts/check.sh's `cargo test --workspace` builds it).
fn borg_bin() -> PathBuf {
    let p = Path::new(env!("CARGO_BIN_EXE_borg-mesh")).with_file_name("borg");
    assert!(p.is_file(), "no borg binary at {}: cargo build -p borg-ai --bin borg", p.display());
    p
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Stubs for every model CLI a hook could reach for: each fails, so nothing costs a token.
fn stub_models(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(root.join("bin")).unwrap();
    for cli in ["claude", "codex", "opencode", "omp"] {
        let p = root.join("bin").join(cli);
        std::fs::write(&p, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// One session between the nodes: B dials A, A serves. Both stamp first, so this is also the round at
/// which each credits the verdicts that arrived on its work (`Node::stamp`).
async fn round(a: &Node, b: &Node) -> (Report, Report) {
    let ticket = a.ticket();
    let (served, synced) = tokio::join!(a.accept(), b.sync(&ticket));
    (served.unwrap(), synced.unwrap())
}

/// No task held by two sessions at once: the tenures of each task never overlap.
fn one_holder_at_a_time(b: &board::Board) {
    for (i, x) in b.spans.iter().enumerate() {
        for y in b.spans[i + 1..].iter().filter(|y| y.task == x.task) {
            assert!(x.to_ms.is_some_and(|t| t <= y.from_ms) || y.to_ms.is_some_and(|t| t <= x.from_ms), "{x:?} overlaps {y:?}");
        }
    }
}

fn verified_of(h: &Path, lesson: &str) -> usize {
    borg::outcome::read(&h.join("outcomes.jsonl")).iter().filter(|o| o.kind == borg::outcome::VERIFIED && o.lessons.iter().any(|l| l == lesson)).count()
}

fn team_verified(h: &Path, lesson: &str) -> u32 {
    let ls = [borg::conscience::Challenge { id: lesson.into(), ..Default::default() }];
    borg::memory::derive(&borg::outcome::read_team(&h.join("lessons.jsonl")), &ls, None)[lesson].verified
}

fn task_stats(h: &Path, kind: &str) -> usize {
    borg::stats::events(h).iter().filter(|e| e.kind == kind).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_opened_on_a_is_claimed_done_and_verified_across_two_nodes() {
    let root = std::env::temp_dir().join(format!("borg-hive-mesh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    stub_models(&root);
    let (ma, mb) = (Machine::new(&root, "a"), Machine::new(&root, "b"));
    // One repo, two checkouts: A's, and B's as a worktree of it (shared objects).
    std::fs::create_dir_all(&ma.app).unwrap();
    git(&ma.app, &["init", "-q", "-b", "main"]);
    std::fs::write(ma.app.join("README"), "app\n").unwrap();
    git(&ma.app, &["add", "-A"]);
    git(&ma.app, &["commit", "-qm", "base"]);
    git(&ma.app, &["worktree", "add", "-q", "--detach", mb.app.to_str().unwrap()]);
    // Alice founds the collective and invites Bob; both hold lesson L1.
    let (sa_store, sb_store) = (ma.store(), mb.store());
    let alice = Identity::load_or_create(&sa_store).unwrap();
    let bob = Identity::load_or_create(&sb_store).unwrap();
    let mut m = Membership::found("work", &alice);
    m.invite(&alice, &bob.public(), "member").unwrap();
    m.save(&sa_store).unwrap();
    m.save(&sb_store).unwrap();
    let l1 = json!({"id": "L1", "origin_ms": 1, "question": "Does the writer close the file?", "version": 1}).to_string() + "\n";
    std::fs::write(sa_store.join("lessons.jsonl"), &l1).unwrap();
    std::fs::write(sb_store.join("lessons.jsonl"), &l1).unwrap();
    let na = Node::bind(&ma.place(), LOCAL.parse().unwrap()).await.unwrap();
    let nb = Node::bind(&mb.place(), LOCAL.parse().unwrap()).await.unwrap();

    // 1. A opens the goal; one round carries it to B, without goal.json.
    ma.ok(&root, &["init", "g1", "--spec", "Write f1", "--oracle", "t=test -f f1.txt", "--budget", "1", "--task", "t1|work|Write f1|f1.txt|"], &[]);
    round(&na, &nb).await;
    assert!(hive::goal_jsonl_path(&sb_store, "g1").is_file() && hive::tasks_path(&sb_store, "g1").is_file());
    assert!(!hive::goal_path(&sb_store, "g1").exists(), "goal.json never travels");
    assert_eq!(hive::goal::load(&sb_store, "g1").unwrap().spec, "Write f1");
    assert_eq!(ma.board(), mb.board());

    // 2. Both nodes' d1 claim t1 before they sync: B's first.
    let (sb, sa) = ("sb-session-0001", "sa-session-0001");
    let d1_b = [("BORG_HIVE", "g1"), ("BORG_HIVE_DRONE", "d1")];
    let d1_a = d1_b;
    mb.prompt(&root, sb, &d1_b);
    ma.prompt(&root, sa, &d1_a);
    // A's oracle is a shell command from another machine: B's drones may not take t1, and nothing
    // runs there, until B's engineer approves it (A's own runs unapproved).
    let (out, code) = mb.hive(&root, &["claim", "t1", "--session", sb], &d1_b);
    assert!(code != 0 && out.contains("not approved on this node") && out.contains("test -f f1.txt"), "{out}");
    assert!(mb.ok(&root, &["board", "g1"], &[]).contains("wait for `borg hive approve g1`"));
    let (out, code) = mb.hive(&root, &["approve", "g1"], &[("BORG_HIVE", "g1")]);
    assert!(code != 0 && out.contains("never by a drone"), "{out}");
    let out = mb.ok(&root, &["approve", "g1"], &[]);
    assert!(out.contains("approve goal t: test -f f1.txt") && out.contains("1 check(s) approved"), "{out}");
    assert!(ma.ok(&root, &["approve", "g1"], &[]).contains("nothing to approve"));
    assert!(mb.ok(&root, &["claim", "t1", "--session", sb], &d1_b).starts_with("claimed t1"));
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(ma.ok(&root, &["claim", "t1", "--session", sa], &d1_a).starts_with("claimed t1"), "A cannot know of B's claim yet");

    // 3. One round: both fold the same board, B's earlier claim holds, A's lost.
    round(&na, &nb).await;
    let (ba, bb) = (ma.board(), mb.board());
    assert_eq!(ba, bb, "the same board on both nodes");
    assert!(matches!(&ba.tasks["t1"].status, Status::Claimed { by, .. } if by == sb), "{:?}", ba.tasks["t1"].status);
    assert_eq!(board::lost_since(&ba, sa, 0), ["t1"]);
    one_holder_at_a_time(&ba);
    assert!(ma.prompt(&root, sa, &d1_a).contains("Your claim on t1 did not take effect"), "A's drone learns at its next hook");

    // 4. B does the work, a lesson shown inside its claim; its done opens t1.v1 on both nodes.
    let claimed = board::read(&hive::tasks_path(&sb_store, "g1")).into_iter().find(|e| e.ev == Ev::Claim && e.by == sb).unwrap().ms;
    let asked = json!({"kind": "asked", "session": sb, "lessons": ["L1"], "ms": claimed + 1, "repo": "app"}).to_string() + "\n";
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(sb_store.join("outcomes.jsonl"))
        .and_then(|mut f| std::io::Write::write_all(&mut f, asked.as_bytes()))
        .unwrap();
    std::fs::write(mb.app.join("f1.txt"), "one\n").unwrap();
    assert!(mb.ok(&root, &["done", "--session", sb], &d1_b).contains("verify task t1.v1 opened"));
    // 8. B tells the verifier of t1.v1, whoever it will be.
    mb.ok(&root, &["tell", "the file format changed: one line per record", "--to", "t1.v1", "--session", sb], &d1_b);
    round(&na, &nb).await;
    let (ba, bb) = (ma.board(), mb.board());
    assert_eq!(ba, bb);
    assert_eq!(ba.tasks["t1.v1"].status, Status::Open);
    assert_eq!(ba.tasks["t1.v1"].task.not_by, [sb, "d1"]);

    // 5. A's d1 is another drone than B's d1: its claim of the verify task takes effect.
    let out = ma.ok(&root, &["claim", "t1.v1", "--session", sa], &d1_a);
    assert!(out.starts_with("claimed t1.v1"), "{out}");
    assert!(matches!(&ma.board().tasks["t1.v1"].status, Status::Claimed { by, .. } if by == sa));
    // 8. The message reaches the verifier once.
    let ctx = ma.prompt(&root, sa, &d1_a);
    assert!(ctx.contains("d1 (node ") && ctx.contains("tells: the file format changed: one line per record"), "{ctx}");
    assert!(!ma.prompt(&root, sa, &d1_a).contains("the file format changed"), "shown once");
    let out = ma.ok(&root, &["verify", "--pass", "--session", sa], &d1_a);
    assert!(out.contains("t1.v1 verified"), "{out}");
    // The verdict reaches B at the first round; B credits it at the next one's stamp, and that
    // round carries its outcome to A.
    round(&na, &nb).await;
    round(&na, &nb).await;

    // 6. Both boards verified and equal; the credit once, on the author's node only.
    let (ba, bb) = (ma.board(), mb.board());
    assert_eq!(ba, bb);
    assert!(matches!(ba.tasks["t1"].status, Status::Verified { .. }) && matches!(ba.tasks["t1.v1"].status, Status::Verified { .. }));
    one_holder_at_a_time(&ba);
    let credited: Vec<borg::outcome::Outcome> = borg::outcome::read(&sb_store.join("outcomes.jsonl")).into_iter().filter(|o| o.kind == "verified").collect();
    assert_eq!(credited.len(), 1, "{credited:?}");
    assert_eq!((credited[0].lessons.as_slice(), credited[0].cause.as_str()), (["L1".to_string()].as_slice(), "g1/t1.v1"));
    assert_eq!(verified_of(&sa_store, "L1"), 0, "A holds no shown record of B's session");
    assert_eq!((team_verified(&sa_store, "L1"), team_verified(&sb_store, "L1")), (1, 1), "one verified per node's memory");
    assert_eq!((task_stats(&sa_store, "task_verified"), task_stats(&sb_store, "task_verified")), (1, 0), "the verifier's node counts it");

    // 7. One more round moves only digests, and nothing is credited again.
    let (served, synced) = round(&na, &nb).await;
    for r in [served, synced] {
        assert_eq!((r.sent, r.received, r.rejected, r.quarantined), (0, 0, 0, 0), "{r:?}");
    }
    assert_eq!(verified_of(&sb_store, "L1"), 1);
    assert_eq!((team_verified(&sa_store, "L1"), team_verified(&sb_store, "L1")), (1, 1));
    na.close().await;
    nb.close().await;
    let _ = std::fs::remove_dir_all(&root);
}

/// The secrets the second test hands the hive: a task title's, a contract's and a message's.
const SECRETS: [&str; 3] = ["ghp_abcdefghijklmnopqrstuvwxyz0123456789", "hunter2hunter2", "sk-ant-api03-abcdefghijklmnopqrstuvwx"];

/// Every file under `dir` that holds `needle`.
fn holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(holding(&p, needle));
        } else if std::fs::read(&p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle)) {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn work_done_on_b_is_verified_and_integrated_on_a_with_its_own_object_store() {
    let root = std::env::temp_dir().join(format!("borg-hive-mesh-commits-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    stub_models(&root);
    let (ma, mb) = (Machine::new(&root, "a"), Machine::new(&root, "b"));
    // One remote, two clones with object stores of their own: nothing B commits is in A's repo.
    let origin = root.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    std::fs::write(origin.join("README"), "app\n").unwrap();
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "-qm", "base"]);
    for m in [&ma, &mb] {
        git(&root, &["clone", "-q", "--no-local", origin.to_str().unwrap(), m.app.to_str().unwrap()]);
    }
    let (sa_store, sb_store) = (ma.store(), mb.store());
    let alice = Identity::load_or_create(&sa_store).unwrap();
    let bob = Identity::load_or_create(&sb_store).unwrap();
    let mut m = Membership::found("work", &alice);
    m.invite(&alice, &bob.public(), "member").unwrap();
    m.save(&sa_store).unwrap();
    m.save(&sb_store).unwrap();
    let na = Node::bind(&ma.place(), LOCAL.parse().unwrap()).await.unwrap();
    let nb = Node::bind(&mb.place(), LOCAL.parse().unwrap()).await.unwrap();

    // 1. A opens the goal, its task's title holding a token; B approves A's check and d1 claims t1.
    let task = format!("t1|work|Write f1, the deploy token {} expired|f1.txt|", SECRETS[0]);
    ma.ok(&root, &["init", "g1", "--spec", "Write f1", "--oracle", "t=test -f f1.txt", "--budget", "1", "--task", &task], &[]);
    round(&na, &nb).await;
    mb.ok(&root, &["approve", "g1"], &[]);
    let (sb, sa) = ("sb-session-0001", "sa-session-0001");
    let d1 = [("BORG_HIVE", "g1"), ("BORG_HIVE_DRONE", "d1")];
    mb.prompt(&root, sb, &d1);
    assert!(mb.ok(&root, &["claim", "t1", "--session", sb], &d1).starts_with("claimed t1"));
    assert!(mb.board().tasks["t1"].task.title.contains("the deploy token [REDACTED] expired"), "the fold reads the redacted title");
    mb.ok(&root, &["contract", &format!("f1.txt is read with password={}", SECRETS[1]), "--scope", "f1.txt", "--session", sb], &d1);
    std::fs::write(mb.app.join("f1.txt"), "one\n").unwrap();
    assert!(mb.ok(&root, &["done", "--session", sb], &d1).contains("verify task t1.v1 opened"));
    mb.ok(&root, &["tell", &format!("the staging key {} works", SECRETS[2]), "--to", "t1.v1", "--session", sb], &d1);
    let events = board::read(&hive::tasks_path(&sb_store, "g1"));
    let attempt = borg::hive::verdict::attempt(&events, &mb.board(), "t1.v1").unwrap();
    let present = |c: &str| {
        Command::new("git").arg("-C").arg(&ma.app).args(["cat-file", "-e", &format!("{c}^{{commit}}")]).stderr(Stdio::null()).status().unwrap().success()
    };
    assert!(!present(&attempt.head), "separate object stores");

    // 2. B's done bundled the attempt. Before it is stamped, B's log holds a forged bundle instead (another
    //    commit under the attempt's names) and one over the cap: A refuses the first and never gets the other.
    let bundles = borg::hive::commits::path(&sb_store, "g1");
    let genuine = std::fs::read_to_string(&bundles).unwrap();
    assert_eq!(genuine.lines().count(), 1, "one bundle per attempt");
    std::fs::write(mb.app.join("f2.txt"), "two\n").unwrap();
    let other = borg::hive::verdict::snapshot(&mb.app, borg::hive::verdict::SNAPSHOT_TIMEOUT).unwrap();
    let forged_ref = "refs/borg/forged";
    git(&mb.app, &["update-ref", forged_ref, &other]);
    let f = root.join("forged.bundle");
    git(&mb.app, &["bundle", "create", "-q", f.to_str().unwrap(), forged_ref, "--not", "--remotes"]);
    let line = |bundle: String| {
        let b = borg::hive::commits::Bundle {
            v: 1,
            task: "t1".into(),
            node: hive::node(&sb_store),
            ms: 1,
            base: attempt.base.clone(),
            head: attempt.head.clone(),
            bundle,
        };
        serde_json::to_string(&b).unwrap()
    };
    use base64::Engine as _;
    let forged = line(base64::engine::general_purpose::STANDARD.encode(std::fs::read(&f).unwrap()));
    let oversized = line("A".repeat(borg::hive::commits::MAX_LINE));
    std::fs::write(&bundles, format!("{forged}\n{oversized}\n")).unwrap();
    round(&na, &nb).await;
    let at_a = std::fs::read_to_string(borg::hive::commits::path(&sa_store, "g1")).unwrap();
    assert_eq!(at_a.lines().collect::<Vec<_>>(), [forged.as_str()], "the oversized line never leaves B");
    ma.prompt(&root, sa, &d1);
    let (out, code) = ma.hive(&root, &["claim", "t1.v1", "--session", sa], &d1);
    assert!(code != 0 && out.contains("its work was done on node") && out.contains("not in this repo"), "{out}");
    assert!(!present(&attempt.head) && !present(&other), "nothing of the forged bundle lands in A's repo");
    let view = borg::hive::view::of(&sa_store, "g1", borg::learn::now_ms()).unwrap();
    assert!(view.commits.iter().any(|c| c.state.starts_with("refused: its commits are")), "{:?}", view.commits);

    // 3. The genuine line arrives after the forged one: A's verifier claims t1.v1 and passes it.
    std::fs::write(&bundles, format!("{forged}\n{oversized}\n{genuine}")).unwrap();
    round(&na, &nb).await;
    assert!(ma.ok(&root, &["claim", "t1.v1", "--session", sa], &d1).starts_with("claimed t1.v1"));
    assert!(present(&attempt.base) && present(&attempt.head) && !present(&other));
    let ctx = ma.prompt(&root, sa, &d1);
    assert!(ctx.contains("tells: the staging key [REDACTED] works"), "{ctx}");
    assert!(ma.ok(&root, &["verify", "--pass", "--session", sa], &d1).contains("t1.v1 verified"));
    let top = ma.borg(&root, &["top"], &[]).output().unwrap();
    let top = String::from_utf8_lossy(&top.stdout);
    assert!(top.contains("commits t1") && top.contains(": unbundled") && top.contains(": refused: its commits are"), "{top}");

    // 4. A integrates: its combined tree holds B's commit, and both boards fold identically.
    round(&na, &nb).await;
    let out = ma.ok(&root, &["integrate", "g1"], &[]);
    assert!(out.contains("integrated") && out.contains("the goal is finished"), "{out}");
    let integrated = git(&ma.app, &["rev-parse", "refs/heads/hive/g1/integrated"]);
    git(&ma.app, &["merge-base", "--is-ancestor", &attempt.head, &integrated]);
    assert_eq!(git(&ma.app, &["show", &format!("{integrated}:f1.txt")]), "one");
    round(&na, &nb).await;
    let (ba, bb) = (ma.board(), mb.board());
    assert_eq!(ba, bb, "the same board on both nodes");
    assert!(ba.finished() && matches!(ba.tasks["t1"].status, Status::Verified { .. }));
    one_holder_at_a_time(&ba);

    // 5. No secret is in any byte either store holds, the synced logs included.
    for s in SECRETS {
        for store in [&sa_store, &sb_store] {
            assert!(holding(store, s).is_empty(), "{s} in {:?}", holding(store, s));
        }
    }
    let (served, synced) = round(&na, &nb).await;
    for r in [served, synced] {
        assert_eq!((r.sent, r.received, r.rejected), (0, 0, 0), "a further round moves only digests: {r:?}");
    }
    na.close().await;
    nb.close().await;
    let _ = std::fs::remove_dir_all(&root);
}

/// Each iteration's cost in the budget test; the goal's budget is four of them.
const ITER_USD: f64 = 0.25;
const BUDGET_USD: f64 = 1.0;

/// `borg hive run g1 --drones <n> --harness sim --yes` on machine `m`, its scripted drones signing as
/// `node`, each doing one task per iteration at `ITER_USD`; its output in `<root>/run-<node>.out`.
fn spawn_run(root: &Path, m: &Machine, node: &str, drones: usize) -> std::process::Child {
    spawn_scripted(root, m, node, drones, json!({"one_task": true, "work_s": 3}))
}

/// `spawn_run` with each drone scripted by `plan` (hive/sim_drone.rs).
fn spawn_scripted(root: &Path, m: &Machine, node: &str, drones: usize, plan: Value) -> std::process::Child {
    let names: serde_json::Map<String, Value> = (1..=drones).map(|n| (format!("d{n}"), plan.clone())).collect();
    let script = m.base.join("script.json");
    std::fs::write(&script, json!({"usd_per_iter": ITER_USD, "node": node, "drones": names}).to_string()).unwrap();
    let out = std::fs::File::create(root.join(format!("run-{node}.out"))).unwrap();
    let (n, dir, script) = (drones.to_string(), m.app.display().to_string(), script.display().to_string());
    let env = [("BORG_TEST_HARNESS", "1"), ("BORG_SIM_CAPS", "prompt,tool,deny,headless"), ("BORG_SIM_SCRIPT", script.as_str()), ("BORG_LEARN", "off")];
    let mut c = m.borg(root, &["hive", "run", "g1", "--drones", &n, "--harness", "sim", "--yes", "--dir", &dir], &env);
    c.stdin(Stdio::null()).stdout(out.try_clone().unwrap()).stderr(out);
    c.spawn().unwrap()
}

fn spend_lines(store: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(hive::run::spend_path(store, "g1")).unwrap_or_default().lines().map(String::from).collect()
}

/// Two machines under `root` (emptied first), each with a clone of one remote and an object store of
/// its own, members of one team, and their mesh nodes on loopback.
async fn two_clones(root: &Path) -> (Machine, Machine, Node, Node) {
    let _ = std::fs::remove_dir_all(root);
    stub_models(root);
    let (ma, mb) = (Machine::new(root, "a"), Machine::new(root, "b"));
    let origin = root.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    std::fs::write(origin.join("README"), "app\n").unwrap();
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "-qm", "base"]);
    for m in [&ma, &mb] {
        git(root, &["clone", "-q", "--no-local", origin.to_str().unwrap(), m.app.to_str().unwrap()]);
    }
    let alice = Identity::load_or_create(&ma.store()).unwrap();
    let bob = Identity::load_or_create(&mb.store()).unwrap();
    let mut m = Membership::found("work", &alice);
    m.invite(&alice, &bob.public(), "member").unwrap();
    m.save(&ma.store()).unwrap();
    m.save(&mb.store()).unwrap();
    let na = Node::bind(&ma.place(), LOCAL.parse().unwrap()).await.unwrap();
    let nb = Node::bind(&mb.place(), LOCAL.parse().unwrap()).await.unwrap();
    (ma, mb, na, nb)
}

/// Waits for `runs` to exit while the mesh syncs every 50 ms, at most `secs` (then stopped as an
/// engineer stops a run, Ctrl-C, and the test fails): their exit codes.
async fn sync_until_exit(runs: &mut [std::process::Child], na: &Node, nb: &Node, secs: u64) -> Vec<i32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut codes: Vec<Option<i32>> = vec![None; runs.len()];
    while codes.iter().any(Option::is_none) {
        for (c, code) in runs.iter_mut().zip(codes.iter_mut()) {
            if code.is_none() {
                *code = c.try_wait().unwrap().map(|s| s.code().unwrap_or(-1));
            }
        }
        if std::time::Instant::now() > deadline {
            for c in runs.iter_mut() {
                let _ = Command::new("kill").args(["-INT", &c.id().to_string()]).status();
                let _ = c.wait();
            }
            panic!("the runs did not end within {secs} s");
        }
        round(na, nb).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    round(na, nb).await;
    round(na, nb).await;
    codes.into_iter().flatten().collect()
}

/// A goal's budget across nodes (tasks/todo.md D1): runs start at once on A (2 drones) and B (1), each
/// splitting the whole budget (neither has seen the other's spend), while the mesh syncs every 50 ms.
/// The goal's spend lines sync, so every drone stops once the goal's total over both nodes reaches the
/// budget: the total is at most the budget plus the iterations in flight then, one per drone (run.rs
/// `ledger`), where each node's own split would have let them spend the budget each.
#[tokio::test(flavor = "multi_thread")]
async fn runs_on_two_nodes_spend_one_goal_budget() {
    let root = std::env::temp_dir().join(format!("borg-hive-mesh-budget-{}", std::process::id()));
    let (ma, mb, na, nb) = two_clones(&root).await;
    let (sa_store, sb_store) = (ma.store(), mb.store());

    // A opens the goal with more tasks than the budget pays for; B approves A's checks.
    ma.ok(&root, &["init", "g1", "--spec", "Write the files", "--oracle", "ok=true", "--budget", &BUDGET_USD.to_string()], &[]);
    for i in 1..=12 {
        let (id, file) = (format!("t{i}"), format!("f{i}.txt"));
        let accept = format!("ok=grep -q 'done {id}' {file}");
        ma.ok(&root, &["open", "g1", "--id", &id, "--title", &format!("write {file}"), "--files", &file, "--accept", &accept], &[]);
    }
    round(&na, &nb).await;
    mb.ok(&root, &["approve", "g1"], &[]);

    let mut runs = [spawn_run(&root, &ma, "a", 2), spawn_run(&root, &mb, "b", 1)];
    sync_until_exit(&mut runs, &na, &nb, 600).await;
    let out: Vec<String> = ["a", "b"].iter().map(|n| std::fs::read_to_string(root.join(format!("run-{n}.out"))).unwrap()).collect();

    // Each run split what it saw left (the whole budget, unless a peer's first lines had arrived): their
    // splits together promised more than the budget, so the goal's stop rule is what bounds them.
    let share = |store: &Path| {
        let text = std::fs::read_to_string(hive::run::runs_path(store, "g1")).unwrap();
        serde_json::from_str::<Value>(text.lines().last().unwrap()).unwrap()["share_usd"].as_f64().unwrap()
    };
    let (share_a, share_b) = (share(&sa_store), share(&sb_store));
    assert!(2.0 * share_a + share_b >= BUDGET_USD + 2.0 * ITER_USD, "shares {share_a} on A, {share_b} on B\n{out:?}");

    // Both nodes hold every node's spend lines, each naming its node.
    let (la, lb) = (spend_lines(&sa_store), spend_lines(&sb_store));
    assert_eq!(la, lb, "the goal's spend synced");
    let lines: Vec<hive::run::Spend> = la.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    let (ida, idb) = (hive::node(&sa_store), hive::node(&sb_store));
    assert!(lines.iter().all(|s| s.node == ida || s.node == idb), "{lines:?}");
    assert!(lines.iter().any(|s| s.node == ida) && lines.iter().any(|s| s.node == idb), "both nodes spent: {lines:?}");
    let total = hive::run::ledger(&lines, &ida);
    assert_eq!(total, hive::run::ledger(&lines, &idb), "both nodes fold one ledger");
    assert_eq!(hive::run::goal_spent(&sa_store, "g1"), hive::run::goal_spent(&sb_store, "g1"));

    // The bound: the budget was reached, and passed by at most one in-flight iteration per drone.
    let drones = 3.0;
    assert!(total.total >= BUDGET_USD, "the drones spent the budget: {total:?}\n{out:?}");
    assert!(total.total <= BUDGET_USD + drones * ITER_USD + 1e-9, "at most one iteration per drone over the budget: {total:?}\n{out:?}");
    assert!(total.total < 2.0 * BUDGET_USD, "less than each node's own split would have let them spend: {total:?}");
    let ends: Vec<&str> = out.iter().flat_map(|o| o.lines()).filter(|l| l.starts_with('d') && l.contains(" end: ")).collect();
    assert_eq!(ends.len(), 3, "{out:?}");
    assert!(ends.iter().all(|l| l.ends_with("end: budget")), "every drone stopped on the goal's budget: {ends:?}");
    assert!(out[1].contains("goal g1: $") && out[1].contains("spent (here $"), "the run says the goal's total by node: {}", out[1]);
    let view = borg::hive::view::of(&sb_store, "g1", borg::learn::now_ms()).unwrap();
    assert!((view.spent_usd - total.total).abs() < 1e-9, "monitoring shows the goal's total: {}", view.spent_usd);
    assert!(view.drones.iter().any(|d| d.drone.starts_with("d2@")), "A's drones under their node on B: {:?}", view.drones);
    na.close().await;
    nb.close().await;
    let _ = std::fs::remove_dir_all(&root);
}

/// A task left to its owner's session that is slow to claim stays in hand on the other node
/// (d3-accept3 W2: the owner's model claimed 12 to 15 s after its session started, while A's idle drone
/// counted idle polls and ended "nothing to do"; the verify task only A could take was stranded and both
/// runs ended unfinished). B owns the one task and its drone claims 15 s into its session, longer than
/// three idle polls; A's drone waits, verifies B's work, and both runs finish the board.
#[tokio::test(flavor = "multi_thread")]
async fn a_peer_slow_to_claim_its_task_is_waited_for() {
    let root = std::env::temp_dir().join(format!("borg-hive-mesh-slow-{}", std::process::id()));
    let (ma, mb, na, nb) = two_clones(&root).await;
    let (ida, idb) = (hive::node(&ma.store()), hive::node(&mb.store()));
    let live = std::collections::BTreeSet::from([ida.as_str(), idb.as_str()]);
    let t = (0..).map(|i| format!("t{i}")).find(|t| board::owner(&live, t) == Some(idb.as_str())).unwrap();
    ma.ok(&root, &["init", "g1", "--spec", "Write the file", "--oracle", "ok=true", "--budget", "5"], &[]);
    let accept = format!("ok=grep -q 'done {t}' f1.txt");
    ma.ok(&root, &["open", "g1", "--id", &t, "--title", "write f1.txt", "--files", "f1.txt", "--accept", &accept], &[]);
    round(&na, &nb).await;
    mb.ok(&root, &["approve", "g1"], &[]);

    // B's session has started on its task (its line reached A) before A's run starts.
    let b_run = spawn_scripted(&root, &mb, "b", 1, json!({"claim_s": 15}));
    let started = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let key = (idb.clone(), "d1".to_string());
    while !ma.board().sessions.get(&key).is_some_and(|(start, settled)| start > settled) {
        assert!(std::time::Instant::now() < started, "B's session start did not reach A");
        round(&na, &nb).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let mut runs = [spawn_scripted(&root, &ma, "a", 1, json!({})), b_run];
    let codes = sync_until_exit(&mut runs, &na, &nb, 300).await;
    let out: Vec<String> = ["a", "b"].iter().map(|n| std::fs::read_to_string(root.join(format!("run-{n}.out"))).unwrap()).collect();
    assert!(!out[0].contains("end: nothing to do"), "A waited for B's claim: {}", out[0]);
    assert_eq!(codes, [0, 0], "both runs finished the board: {out:?}");
    let (ba, bb) = (ma.board(), mb.board());
    assert!(ba.finished() && bb.finished(), "{out:?}");
    let node_of = |b: &board::Board, id: &str| b.spans.iter().find(|sp| sp.task == id).map(|sp| sp.node.clone());
    assert_eq!((node_of(&ba, &t), node_of(&ba, &format!("{t}.v1"))), (Some(idb.clone()), Some(ida.clone())), "B did the work, A verified it");
    na.close().await;
    nb.close().await;
    let _ = std::fs::remove_dir_all(&root);
}
