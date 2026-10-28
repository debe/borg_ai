//! `/api/learning` over a seeded team store, in process (no port): lessons written per source and week
//! from this machine's events, the asked→changed and repeat-veto counts from every node's rollups, each
//! section labelled with where its numbers come from. Nothing is stored for the view: it folds the logs as they are.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
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

fn seed(home: &std::path::Path) {
    std::fs::create_dir_all(home.join("stats")).unwrap();
    // This machine's events: lessons written by source, and one asked lesson that changed the work.
    let ev = |ms: i64, kind: &str, why: &str| json!({"ms": ms, "kind": kind, "session": "s1", "why": why});
    let here = [
        ev(WEEK_A, "lesson_new", "session"),
        ev(WEEK_A + 1, "lesson_new", "veto"),
        ev(WEEK_B, "lesson_new", "review"),
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
    // Two lessons learned in week B.
    let lesson = |id: &str, episodes: &[&str]| Lesson {
        id: id.into(),
        origin_ms: WEEK_B,
        question: format!("question {id}?"),
        episodes: episodes.iter().map(|e| e.to_string()).collect(),
        version: 1,
        ..Default::default()
    };
    let lines = [lesson("L1", &["e-1"]), lesson("L2", &["e-2"])].iter().map(|l| serde_json::to_string(l).unwrap() + "\n").collect::<String>();
    std::fs::write(home.join("lessons.jsonl"), lines).unwrap();
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
    assert_eq!(src, ["session", "review", "veto"]);
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
    assert!(!home.join("learning").exists(), "the view keeps no state of its own");
    let _ = std::fs::remove_dir_all(&home);
}
