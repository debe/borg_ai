//! The hive across machines (docs/design/collective.md step 4), proven on two loopback mesh nodes in
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

    /// The `borg` binary with this machine's environment and `env`, in its checkout.
    fn borg(&self, root: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut c = Command::new(borg_bin());
        c.args(args)
            .env_clear()
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
