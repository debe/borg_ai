//! `/api/learning` over a seeded team store, in process (no port): lessons written per source and week
//! from this machine's events, the asked→changed and repeat-veto counts from every node's rollups, a hive
//! goal's caught mistakes from its board and the rework lessons learned from them, each section labelled
//! with where its numbers come from. Nothing is stored for the view: it folds the logs as they are.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg::hive::{self, Ev, Event, Goal, Role, Task, board, goal};
use borg::learn::Lesson;
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Noon UTC on Tuesday 2026-10-13 (week of Monday 2026-10-12) and Wednesday 2026-10-21 (week of 10-19).
/// The view runs to the current week, so the assertions read the first weeks only.
const WEEK_A: i64 = 1_791_892_800_000;
const WEEK_B: i64 = 1_792_584_000_000;
/// A week before week A (week of Monday 2026-10-05).
const WEEK_0: i64 = WEEK_A - 7 * 86_400_000;

fn task(id: &str, role: Role) -> Task {
    Task {
        id: id.into(),
        role,
        title: format!("do {id}"),
        repo: String::new(),
        files: vec![format!("{id}.txt")],
        deps: vec![],
        accept: vec![],
        of: None,
        not_by: vec![],
        flags: vec![],
        from: None,
        group: None,
    }
}

fn seed(home: &std::path::Path) {
    std::fs::create_dir_all(home.join("stats")).unwrap();
    // This machine's events: lessons written by source, and one asked lesson that changed the work.
    let ev = |ms: i64, kind: &str, why: &str| json!({"ms": ms, "kind": kind, "session": "s1", "why": why});
    let here = [
        ev(WEEK_A, "lesson_new", "session"),
        ev(WEEK_A + 1, "lesson_new", "veto"),
        ev(WEEK_B, "lesson_new", "rework"),
        ev(WEEK_B + 1, "lesson_new", "session"),
        ev(WEEK_B + 2, "lesson_merged", "session"),
        ev(WEEK_B + 3, "changed", ""),
    ];
    std::fs::write(borg::stats::events_path(home), here.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    // The history derived before live counting began, a week before week A: a veto and a lesson. It
    // derives no merges, repeats or sources, so those are unknown (null) there, never 0.
    let old = [ev(WEEK_0, "veto", ""), ev(WEEK_0 + 1, "lesson_new", "session")];
    std::fs::write(home.join("stats/backfill.jsonl"), old.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    // A peer's rollup in week A: three asked lessons changed the work, one was ignored, one corrected
    // anyway; two vetoes, one the same "no" again.
    let peer = json!({"id": "stat:peer:b@x:2026-10-13", "version": 1, "day": "2026-10-13", "node": "peer", "engineer": "b@x",
        "counts": {"changed": 3, "ignored": 1, "corrected": 1, "veto": 2, "veto_repeat": 1, "lesson_new": 1}, "usd": 0.0, "dur_ms": 0});
    std::fs::write(home.join("stats.jsonl"), format!("{peer}\n")).unwrap();
    // A lesson learned from goal g's rework of t1, and one that has nothing to do with the hive.
    let lesson = |id: &str, episodes: &[&str]| Lesson {
        id: id.into(),
        origin_ms: WEEK_B,
        question: format!("question {id}?"),
        episodes: episodes.iter().map(|e| e.to_string()).collect(),
        version: 1,
        ..Default::default()
    };
    let lines = [lesson("L1", &["rework-g.t1"]), lesson("L2", &["e-1"])].iter().map(|l| serde_json::to_string(l).unwrap() + "\n").collect::<String>();
    std::fs::write(home.join("lessons.jsonl"), lines).unwrap();
    // Goal g: in week A the verifier rejected t1 once, a hacked attempt was caught and an unchecked
    // rejection (no snapshot) says nothing; in week B t1 was verified, then an integration failed.
    let g = Goal {
        v: 1,
        id: "g".into(),
        spec: "Write the files".into(),
        oracle: vec![goal::Check::parse("ok=true", 60).unwrap()],
        budget_usd: 3.0,
        repos: vec![],
        author: "e@x".into(),
        created_ms: 1,
        sig: String::new(),
        hidden: None,
        held_out: false,
        origin: Vec::new(),
    };
    goal::create(home, &g).unwrap();
    let tasks = hive::tasks_path(home, "g");
    // One round of t1: claimed by s-w, done (opening its verify task vN in the same line's session
    // and ms), the verify task claimed by s-ver and judged by `verdict`.
    let round = |n: u32, ms: i64, verdict: Event| -> Vec<Event> {
        let v = Task { of: Some("t1".into()), not_by: vec!["s-w".into()], ..task(&format!("t1.v{n}"), Role::Verify) };
        vec![
            Event::new(Ev::Claim, "t1", "s-w", ms - 4),
            Event::new(Ev::Done, "t1", "s-w", ms - 3),
            Event::open(v, "s-w", ms - 3),
            Event::new(Ev::Claim, &format!("t1.v{n}"), "s-ver", ms - 2),
            verdict,
        ]
    };
    let mut lines = vec![Event::open(task("t1", Role::Work), "engineer:e", WEEK_A - 10)];
    lines.extend(round(1, WEEK_A, Event::new(Ev::Rejected, "t1.v1", "s-ver", WEEK_A).with_note("misses the edge case")));
    // A stale verifier's line on the judged t1.v1 and a line with a credit `record` never writes: ignored.
    lines.push(Event::new(Ev::Rejected, "t1.v1", "s-old", WEEK_A + 1).with_note("late"));
    lines.extend(round(
        2,
        WEEK_A + 10,
        Event { hacked: Some(true), ..Event::new(Ev::Rejected, "t1.v2", "s-ver", WEEK_A + 10).with_note("protected files changed") },
    ));
    lines.extend(round(3, WEEK_A + 20, Event::new(Ev::Rejected, "t1.v3", "s-ver", WEEK_A + 20).with_note(borg::hive::verdict::UNCHECKED)));
    let verified = Event::new(Ev::Verified, "t1.v4", "s-ver", WEEK_B);
    lines.extend(round(4, WEEK_B, verified.clone()));
    // The same verdict line delivered twice counts once.
    lines.push(verified);
    lines.push(Event { credit: "forged".into(), ..Event::new(Ev::Verified, "t1.v4", "s-ver", WEEK_B + 1) });
    lines.extend([
        Event::open(task("integrate.1", Role::Integrate), "engineer:e", WEEK_B + 1),
        Event::new(Ev::Claim, "integrate.1", "s-int", WEEK_B + 1),
        Event::new(Ev::Rejected, "integrate.1", "s-int", WEEK_B + 2).with_note("visible fails"),
    ]);
    // A verdict without a claim: ignored.
    lines.push(Event::new(Ev::Rejected, "integrate.1", "s-x", WEEK_B + 3).with_note("never claimed"));
    board::append(&tasks, &lines).unwrap();
}

#[tokio::test]
async fn the_learning_view_shows_per_week_whether_learning_works() {
    let home = std::env::temp_dir().join(format!("borg-web-learning-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    seed(&home);
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Team, mode: "hook" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), stop });
    let res = router(app).oneshot(Request::get("/api/learning?c=corp").header("host", "127.0.0.1:7979").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();

    assert_eq!(v["share"], "team");
    let first = |x: &Value| Value::Array(x.as_array().unwrap().iter().take(3).cloned().collect());
    assert_eq!(first(&v["weeks"]), json!(["2026-10-05", "2026-10-12", "2026-10-19"]), "Mondays from the first counted week: {v}");
    let weeks = v["weeks"].as_array().unwrap();
    let today = borg::stats::day(borg::learn::now_ms());
    assert!(weeks.last().unwrap().as_str().unwrap() <= today.as_str() && v["partial_week"] == true, "the weeks run to the current one: {v}");
    assert_eq!(v["live_from"], "2026-10-12");
    // Lessons by source come from this machine's live events (they keep the source), most first;
    // the derived week before live counting is unknown.
    let l = &v["lessons"];
    assert_eq!(l["scope"], "this machine");
    assert_eq!(l["by_source"][0]["source"], "session");
    assert_eq!((&l["by_source"][0]["total"], first(&l["by_source"][0]["weeks"])), (&json!(2), json!([null, 1, 1])));
    let src: Vec<&str> = l["by_source"].as_array().unwrap().iter().map(|s| s["source"].as_str().unwrap()).collect();
    assert_eq!(src, ["session", "rework", "veto"]);
    // The team's new lessons are the lessons themselves, each once; merges and verdicts fold every
    // node's rollup (the peer's and this node's); merges are unknown before live counting.
    assert_eq!(l["team"]["scope"], "team");
    assert_eq!((first(&l["team"]["new"]), first(&l["team"]["merged"])), (json!([0, 0, 2]), json!([null, 0, 1])));
    let a = &v["asked"];
    assert_eq!(
        (first(&a["changed"]), first(&a["ignored"]), first(&a["corrected"]), first(&a["not_corrected"])),
        (json!([0, 3, 1]), json!([0, 1, 0]), json!([0, 1, 0]), json!([0, 0, 0]))
    );
    // The derived veto counts; whether it repeated an earlier no is unknown.
    assert_eq!((first(&v["vetoes"]["veto"]), first(&v["vetoes"]["repeat"])), (json!([1, 2, 0]), json!([null, 1, 0])));
    // Goal g's caught mistakes per week, only the verdicts the board took: the unchecked rejection,
    // a stale verifier's, a duplicate, a forged credit and an unclaimed one are left out.
    let g = &v["hive"]["goals"][0];
    assert_eq!(v["hive"]["scope"], "team");
    assert_eq!(g["goal"], "g");
    assert_eq!(
        (first(&g["rejected"]), first(&g["hacked"]), first(&g["verified"]), first(&g["integration_failed"])),
        (json!([0, 1, 0]), json!([0, 1, 0]), json!([0, 0, 1]), json!([0, 0, 1])),
        "{g}"
    );
    let rework: Vec<&str> = g["rework_lessons"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(rework, ["L1"]);
    assert!(v["playbooks"]["list"].is_array());
    assert!(!home.join("learning").exists(), "the view keeps no state of its own");
    let _ = std::fs::remove_dir_all(&home);
}
