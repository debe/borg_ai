//! The page's hive API over a seeded store, in process (no port): a goal's board, budget, spend per
//! drone and contracts in `/api/hive`, written through the board's own API (hive/board.rs). The runner
//! and the hooks write these lines; crates/borg/tests/hive_run_cli.rs proves that end to end.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg::hive::{self, Contract, Ev, Event, Goal, Role, Task, board, goal};
use borg::learn;
use borg_web::{App, Mind, router};
use serde_json::Value;
use tower::ServiceExt;

async fn get(app: Arc<App>, uri: &str) -> (StatusCode, Value) {
    let res = router(app).oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn app(minds: Vec<(&str, PathBuf)>) -> Arc<App> {
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let minds: Vec<Mind> = minds.into_iter().map(|(n, home)| Mind { name: n.into(), home, share: Share::Local, mode: "live" }).collect();
    Arc::new(App { first: minds[0].name.clone(), minds, stop })
}

fn seed(home: &Path, now: i64) {
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
    };
    goal::create(home, &g).unwrap();
    let t = |id: &str| Task {
        id: id.into(),
        role: Role::Work,
        title: format!("write {id}"),
        repo: String::new(),
        files: vec![format!("{id}.txt")],
        deps: vec![],
        accept: vec![],
        of: None,
        not_by: vec![],
        flags: vec![],
    };
    let tasks = hive::tasks_path(home, "g");
    board::append(&tasks, &[Event::open(t("t1"), "engineer:e", now - 10), Event::open(t("t2"), "engineer:e", now - 10)]).unwrap();
    board::append(&tasks, &[Event::new(Ev::Claim, "t1", "sess-1", now - 5).with_drone("d1")]).unwrap();
    let c = Contract::new("t1", "sess-1", now - 4, vec!["t1.txt".into()], "t1.txt holds one line", None);
    hive::contract::publish(home, "g", &c, "app").unwrap();
    let spend = hive::run::spend_path(home, "g");
    std::fs::create_dir_all(spend.parent().unwrap()).unwrap();
    let line =
        |d: &str, usd: f64| serde_json::json!({"v": 1, "ms": now, "drone": d, "iter": 1, "harness": "claude", "usd": usd, "session": "sess-1", "exit": "ok"});
    std::fs::write(&spend, format!("{}\n{}\n", line("d1", 0.4), line("d2", 0.2))).unwrap();
}

#[tokio::test]
async fn a_goal_shows_its_counts_holder_and_spend() {
    let base = std::env::temp_dir().join(format!("borg-web-hive-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (corp, plain) = (base.join("corp"), base.join("plain"));
    std::fs::create_dir_all(&plain).unwrap();
    seed(&corp, learn::now_ms());
    let app = app(vec![("corp", corp), ("plain", plain)]);

    let (status, v) = get(app.clone(), "/api/hive?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["collective"], "corp");
    let g = &v["goals"][0];
    assert_eq!((g["goal"].as_str(), g["counts"]["claimed"].as_u64(), g["counts"]["open"].as_u64()), (Some("g"), Some(1), Some(1)), "{v}");
    let t1 = g["tasks"].as_array().unwrap().iter().find(|t| t["id"] == "t1").unwrap();
    assert_eq!(t1["holder"], "sess-1");
    assert!(t1["lease_left_ms"].as_i64().unwrap() > 0);
    assert!((g["spent_usd"].as_f64().unwrap() - 0.6).abs() < 1e-9, "{g}");
    assert_eq!(g["drones"][0]["drone"], "d1");
    assert_eq!(g["contracts"][0]["text"], "t1.txt holds one line");

    let (status, v) = get(app.clone(), "/api/hive?c=plain").await;
    assert_eq!((status, v["goals"].as_array().map(Vec::len)), (StatusCode::OK, Some(0)), "no hive, no goals");
    let (status, _) = get(app, "/api/hive?c=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&base);
}
