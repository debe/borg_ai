//! The session-end judge in batches (0.7.6) through the binary: `borg learn --session` judges every
//! open delivery, in batches whose prompt stays within the judge's budget, the lessons' part within half
//! of it, the batches' calls in parallel; a long session's work is cut at each batch's earliest delivery
//! (`BORG_END_WORK`), a short one read whole in every batch; a failed batch leaves its lessons alone
//! failed. A fake `claude` on PATH (the prompt arrives on stdin) and a temp store: no real model call.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

/// The start budget (hook.rs `END_START_BUDGET`) while no evidence of the judge's effective length is in
/// the store.
const BUDGET: usize = 72_000;

/// A store of `n` lessons (`L000`…), each `question_chars` long, and a session `s1` that delivered each
/// free in a delivery of its own, an edit line of `edit_chars` after each (`edit-<i>`), and a prompt
/// before each. The fake model answers `changed` for every lesson its prompt names and fails a prompt
/// naming the lesson in the file `fail`. With the file `hold`, a session-end call (except one naming the
/// lesson in `fast`) notes itself in `held/` and waits for the file `release` (at most 2 minutes), so a
/// test synchronises on the calls instead of on the clock.
fn setup(name: &str, n: usize, question_chars: usize, edit_chars: usize) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-end-batches-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let corp = root.join("store/collectives/corp");
    for d in [corp.join("state"), root.join("corp"), root.join("home"), root.join("bin"), root.join("calls"), root.join("held")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let lessons: String = (0..n)
        .map(|i| {
            let q = format!("Is case {i:03} handled? {}", "q".repeat(question_chars));
            format!("{}\n", json!({"id": format!("L{i:03}"), "origin_ms": 1, "question": q, "consequence": format!("case {i:03} breaks")}))
        })
        .collect();
    std::fs::write(corp.join("lessons.jsonl"), lessons).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("corp")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    let (mut state, mut edits) = (String::new(), String::new());
    for i in 0..n {
        let check = format!("{:012x}", i + 1);
        state.push_str(&format!("prompt-{i:03} please handle case {i:03}\n#borg-queued L{i:03} @{check}\n"));
        edits.push_str(&format!("#borg-work work before {i:03}\n#borg-queued L{i:03} @{check}\nedit-{i:03} {}\n", "e".repeat(edit_chars)));
    }
    std::fs::write(corp.join("state/s1.ctx"), state).unwrap();
    std::fs::write(corp.join("state/s1.edits"), edits).unwrap();
    let fake = root.join("bin/claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\np=$(cat)\nf=$(mktemp {calls}/call.XXXXXX)\nprintf '%s' \"$p\" > \"$f\"\n\
             case \"$p\" in *'session has ended'*) ;; *) echo '{{\"structured_output\":{{}}}}'; exit 0;; esac\n\
             if [ -f {root}/hold ] && ! {{ [ -f {root}/fast ] && printf '%s\\n' \"$p\" | grep -q \"^\\[$(cat {root}/fast)\\] lesson\"; }}; then\n\
             mktemp {root}/held/h.XXXXXX >/dev/null; k=0\n\
             while [ ! -f {root}/release ] && [ $k -lt 2400 ]; do sleep 0.05; k=$((k+1)); done; fi\n\
             if [ -f {root}/fail ] && printf '%s\\n' \"$p\" | grep -q \"^\\[$(cat {root}/fail)\\] lesson\"; then exit 1; fi\n\
             out=''\nfor i in $(printf '%s\\n' \"$p\" | sed -n 's/^\\[\\([A-Za-z0-9]*\\)\\] lesson:.*/\\1/p'); do out=\"$out{{\\\"id\\\":\\\"$i\\\",\\\"verdict\\\":\\\"changed\\\",\\\"reason\\\":\\\"r\\\"}},\"; done\n\
             printf '{{\"structured_output\":{{\"resolved\":[%s]}},\"total_cost_usd\":0.01}}' \"${{out%,}}\"\n",
            calls = root.join("calls").display(),
            root = root.display(),
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

/// `borg learn --session s1` as the end hook starts it, in the session's collective, with `env`; the
/// budget pinned to the start (`BORG_END_BUDGET`) unless `env` names it (empty: the measured one).
fn learn_end(root: &Path, env: &[(&str, &str)]) {
    learn_end_started(root, env).wait().unwrap();
}

/// `learn_end`, started and left running.
fn learn_end_started(root: &Path, env: &[(&str, &str)]) -> std::process::Child {
    let corp = root.join("store/collectives/corp");
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["learn", "--session", "s1"])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        .env("BORG_PREDICT_MODEL", "claude:")
        .env("BORG_LEARN_MODEL", "off")
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", &corp)
        .env("BORG_STATE", corp.join("state"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_HARNESS", "claude")
        .env("BORG_TRANSCRIPT_CAPTURE", "off")
        .env("BORG_REVIEW_FIX", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_EARLY", "off")
        .env("BORG_PREFILTER", "off")
        .env("BORG_END_BUDGET", BUDGET.to_string())
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c.envs(env.iter().copied());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"").unwrap();
    child
}

fn file(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join("store/collectives/corp").join(name)).unwrap_or_default()
}

/// The session-end prompts the fake model got, in no particular order.
fn prompts(root: &Path) -> Vec<String> {
    std::fs::read_dir(root.join("calls"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .filter(|p| p.contains("session has ended"))
        .collect()
}

/// The lesson ids a prompt names (`[L007] lesson: …`).
fn named(prompt: &str) -> Vec<String> {
    prompt.lines().filter_map(|l| l.strip_prefix('[')?.split_once("] lesson:").map(|(id, _)| id.to_string())).collect()
}

/// The session-end judge's cost lines.
fn end_lines(root: &Path) -> Vec<String> {
    file(root, "state/s1.cost").lines().filter(|l| l.starts_with("judge ") && l.contains(" at=end")).map(String::from).collect()
}

fn field<'l>(line: &'l str, k: &str) -> &'l str {
    line.split_whitespace().find_map(|w| w.strip_prefix(k)).unwrap_or_else(|| panic!("no {k}: {line}"))
}

/// The lessons' part of a prompt (predict.rs `judge_section`): from the judge's own section on, after
/// the work.
fn lessons_part(prompt: &str) -> usize {
    prompt[prompt.rfind("\n\n## Lessons from earlier in this session\n").unwrap() + 1..].chars().count()
}

/// 278 open deliveries (this session's count, 2026-11-01), 0.7.5's one call saw 40: every one is judged
/// across batches, none left for lack of slots; no batch prompt exceeds the budget and the lessons of
/// one take at most half; each batch's cost line names its batch, budget, model and cut, one
/// `model_call` per batch; with `after`, a long session's work is cut from the batch's earliest delivery on.
#[test]
fn every_open_delivery_is_judged_across_batches_within_the_budget() {
    let root = setup("all", 278, 60, 150);
    learn_end(&root, &[("BORG_END_WORK", "after")]);
    let state = file(&root, "state/s1.ctx");
    assert!(!state.contains("why=slots") && !state.contains("why=failed"), "{state}");
    for i in 0..278 {
        assert!(state.contains(&format!("#borg-judged L{i:03}=changed\n")), "L{i:03} judged");
    }
    let ended: Vec<&str> = state.lines().filter_map(|l| l.strip_prefix("#borg-end-judged ")).collect();
    assert_eq!(ended.iter().flat_map(|l| l.split(',')).count(), 278, "every lesson settled at the end once");
    let ps = prompts(&root);
    let lines = end_lines(&root);
    assert!(ps.len() > 1 && ps.len() == lines.len() && lines.len() == ended.len(), "{} prompts, {} lines", ps.len(), lines.len());
    for p in &ps {
        assert!(p.chars().count() <= BUDGET, "{} chars", p.chars().count());
        assert!(lessons_part(p) <= BUDGET / 2, "lessons {} chars", lessons_part(p));
        // `after`: the edits part starts at the batch's earliest delivery (its first lesson's edit).
        let first = named(p).into_iter().min().unwrap();
        let edits = p.split("### The agent's edits and answers\n").nth(1).unwrap();
        let edit = format!("edit-{}", &first[1..]);
        // Or, near the session's end, where what follows fits: its newest share, which holds it.
        let newest = edits.contains(&edit) && edits.contains("edit-277");
        assert!(edits.trim_start_matches('…').starts_with(&edit) || newest, "{first}: {}", &edits[..80]);
    }
    let n = lines.len();
    // Each batch records as it answers: its place among the batches is `batch=`.
    let mut places: Vec<String> = lines.iter().map(|l| field(l, "batch=").to_string()).collect();
    places.sort_by_key(|p| p.split('/').next().unwrap().parse::<usize>().unwrap());
    assert_eq!(places, (1..=n).map(|i| format!("{i}/{n}")).collect::<Vec<_>>());
    for l in &lines {
        assert_eq!((field(l, "budget="), field(l, "model="), field(l, "cut=")), ("72000", "claude:sonnet", "after"), "{l}");
        assert!(field(l, "chars=").parse::<usize>().unwrap() <= BUDGET, "{l}");
    }
    let calls = file(&root, "stats/events.jsonl").lines().filter(|l| l.contains("\"model_call\"") && l.contains("\"judge\"")).count();
    assert_eq!(calls, n, "one model_call per batch");
    // Settled: a second pass judges nothing.
    learn_end(&root, &[("BORG_END_WORK", "after")]);
    assert_eq!(prompts(&root).len(), n);
    let _ = std::fs::remove_dir_all(root);
}

/// The other cuts: `newest` reads the session's end, `ends` (the default, also for an unknown word) half
/// from the batch's earliest delivery and half the end; each named on the cost line.
#[test]
fn the_work_cut_is_switchable() {
    for (cut, name) in [("newest", "newest"), ("ends", "ends"), ("", "ends"), ("bogus", "ends")] {
        let root = setup(&format!("cut-{cut}"), 120, 60, 400);
        learn_end(&root, &[("BORG_END_WORK", cut)]);
        let lines = end_lines(&root);
        assert!(lines.len() > 1 && lines.iter().all(|l| [name, "none"].contains(&field(l, "cut="))), "{lines:?}");
        assert!(lines.iter().any(|l| field(l, "cut=") == name), "{lines:?}");
        for p in prompts(&root) {
            let first = named(&p).into_iter().min().unwrap();
            let edits = p.split("### The agent's edits and answers\n").nth(1).unwrap();
            let edits = &edits[..edits.find("\n### ").unwrap_or(edits.len())];
            if !edits.contains('…') {
                // What follows the batch's delivery fits beside its lessons: whole.
                continue;
            }
            assert!(edits.contains("edit-119"), "{cut}: the session's end");
            let edit = format!("edit-{}", &first[1..]);
            let starts = edits.trim_start_matches('…').starts_with(&edit);
            match name {
                // Half from the delivery on, or the newest share when the halves meet, which holds it.
                "ends" => assert!(starts || edits.contains(&edit), "{first}: {}", &edits[..60]),
                // The end alone: the first batch's delivery is long gone.
                _ => assert!(first != "L000" || !edits.contains(&edit), "{first}: {}", &edits[..60]),
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A short session's work fits beside every batch's lessons: read whole in each, nothing cut.
#[test]
fn a_short_sessions_work_is_read_whole_in_every_batch() {
    let root = setup("short", 30, 2_500, 20);
    learn_end(&root, &[]);
    let ps = prompts(&root);
    assert!(ps.len() > 1, "{} batches", ps.len());
    for p in &ps {
        assert!(p.contains("prompt-000") && p.contains("prompt-029") && p.contains("edit-000") && p.contains("edit-029"), "whole");
        assert!(!p.split("\n\n## Lessons from earlier").next().unwrap().contains('…'), "nothing cut");
    }
    assert!(end_lines(&root).iter().all(|l| field(l, "cut=") == "none"));
    let _ = std::fs::remove_dir_all(root);
}

/// A failed batch is no try for its own lessons alone (`why=failed`); the other batches record.
#[test]
fn a_failed_batch_leaves_only_its_lessons_failed() {
    let root = setup("fail", 60, 1_500, 20);
    std::fs::write(root.join("fail"), "L031").unwrap();
    learn_end(&root, &[]);
    let state = file(&root, "state/s1.ctx");
    let failed: Vec<&str> = state.lines().filter_map(|l| l.strip_prefix("#borg-unjudged ")?.strip_suffix(" why=failed")).collect();
    assert_eq!(failed.len(), 1, "{state}");
    let failed: Vec<&str> = failed[0].split(',').collect();
    assert!(failed.contains(&"L031") && failed.len() < 60, "{failed:?}");
    for i in 0..60 {
        let id = format!("L{i:03}");
        assert_eq!(state.contains(&format!("#borg-judged {id}=changed\n")), !failed.contains(&id.as_str()), "{id}");
    }
    assert!(prompts(&root).len() > 2);
    let _ = std::fs::remove_dir_all(root);
}

/// Waits until `done` holds, at most 2 minutes (a guard against a hang, not a timing bound).
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !done() {
        assert!(started.elapsed() < std::time::Duration::from_secs(120), "never: {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The batches' calls run `runner::JOBS` (4) at a time: every batch's call is under way before any of
/// them may answer; learning or the consequence model off, nothing runs.
#[test]
fn the_batches_run_in_parallel_and_not_at_all_when_off() {
    let root = setup("parallel", 30, 3_000, 20);
    for env in [("BORG_LEARN", "off"), ("BORG_PREDICT_MODEL", "off")] {
        learn_end(&root, &[env]);
        assert!(prompts(&root).is_empty(), "{env:?}: nothing runs");
        assert!(!file(&root, "state/s1.ctx").contains("#borg-end-judged"), "{env:?}");
    }
    std::fs::write(root.join("hold"), "").unwrap();
    let mut child = learn_end_started(&root, &[]);
    // The pass makes 3-4 batches, all within one round of JOBS: a sequential pass would never have a
    // second call waiting while the first is held.
    until("three calls held at once", || std::fs::read_dir(root.join("held")).unwrap().count() >= 3);
    std::fs::write(root.join("release"), "").unwrap();
    child.wait().unwrap();
    let n = prompts(&root).len();
    assert!((3..=4).contains(&n), "{n} batches");
    assert_eq!(std::fs::read_dir(root.join("held")).unwrap().count(), n, "every call was under way at once");
    let _ = std::fs::remove_dir_all(root);
}

/// A batch records as soon as its call answers, while slower batches still run: a pass stopped partway
/// keeps the verdicts it already has (and paid for).
#[test]
fn each_batch_records_as_it_answers() {
    let root = setup("arrival", 30, 3_000, 20);
    std::fs::write(root.join("fast"), "L000").unwrap();
    std::fs::write(root.join("hold"), "").unwrap();
    let mut child = learn_end_started(&root, &[]);
    // The other batches wait for `release`, so the pass is still running when the first one records.
    until("the first batch recorded", || file(&root, "state/s1.ctx").contains("#borg-end-judged L000"));
    let seen = child.try_wait().unwrap().is_none();
    std::fs::write(root.join("release"), "").unwrap();
    child.wait().unwrap();
    assert!(seen, "the first batch recorded while the others still ran: {}", file(&root, "state/s1.ctx"));
    assert_eq!(file(&root, "state/s1.ctx").matches("#borg-end-judged ").count(), prompts(&root).len(), "every batch recorded");
    let _ = std::fs::remove_dir_all(root);
}

/// The session-end judge's cost lines of an earlier session: `sent` lessons in a prompt of `chars`,
/// `answered` with a verdict other than unclear, in ten calls of equal shares.
fn evidence(chars: usize, sent: u64, answered: u64) -> String {
    let (n, a) = (sent / 10, answered / 10);
    format!(
        "judge call=1 pending={n} resolved={a} chars={chars} returned= unmatched= verdicts=changed:{a},ignored:0,not_arisen:0,consequence_remains:0,unclear:{} reason_chars=1 model=claude:sonnet at=end batch=1/1 budget=72000 cut=after\n",
        n - a
    )
    .repeat(10)
}

/// Unpinned, the budget is the judge's measured effective length (judge_length.rs) from the store's cost
/// lines: no evidence keeps the start and explores the step above on a drawn share of passes; a flat
/// share up to 108k moves it there; a share that falls by 72k moves it down to 48k, the failed 72k
/// explored at the floor's chance.
/// Every pass logs its chance (`psi=`) and draw (`explore=`), every batch the budget it kept to.
#[test]
fn the_budget_is_the_judges_measured_effective_length_with_its_draw_logged() {
    let cases: [(&str, String, [&str; 2], &str); 3] = [
        ("none", String::new(), ["72000", "108000"], "0.2000"),
        ("flat", evidence(30_000, 400, 200) + &evidence(70_000, 300, 150) + &evidence(100_000, 300, 150), ["108000", "162000"], "0.2000"),
        ("worse", evidence(30_000, 400, 300) + &evidence(45_000, 300, 230) + &evidence(70_000, 300, 120), ["48000", "72000"], "0.0500"),
    ];
    for (name, lines, [kept, explored], psi) in cases {
        let root = setup(&format!("length-{name}"), 120, 60, 400);
        std::fs::write(root.join("store/collectives/corp/state/earlier.cost"), lines).unwrap();
        learn_end(&root, &[("BORG_END_BUDGET", "")]);
        let ls = end_lines(&root);
        assert!(
            !ls.is_empty(),
            "{name}: {}\n{}",
            file(&root, "state/s1.cost"),
            file(&root, "state/s1.ctx").lines().filter(|l| l.starts_with("#borg-un")).collect::<Vec<_>>().join("\n")
        );
        let draw = field(&ls[0], "explore=");
        for l in &ls {
            assert_eq!((field(l, "psi="), field(l, "explore=")), (psi, draw), "{name}: one draw per pass: {l}");
            let budget = if draw == "1" { explored } else { kept };
            assert_eq!(field(l, "budget="), budget, "{name}: {l}");
            assert!(field(l, "chars=").parse::<usize>().unwrap() <= budget.parse().unwrap(), "{name}: {l}");
        }
        for p in prompts(&root) {
            let budget: usize = if draw == "1" { explored } else { kept }.parse().unwrap();
            assert!(p.chars().count() <= budget, "{name}: {} chars", p.chars().count());
        }
        // The counts are cached in the store's state, beside the cost lines, as the pass read them.
        let cache = file(&root, "state/judge-length.json");
        assert!(cache.contains("\"version\":2") && (name == "none") != cache.contains("\"claude:sonnet\""), "{name}: {cache}");
        let _ = std::fs::remove_dir_all(root);
    }
}
