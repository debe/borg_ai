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
    get_as(app, uri, Some("127.0.0.1:7979")).await
}

/// GET `uri` with `host` as its Host header (none when None).
async fn get_as(app: Arc<App>, uri: &str, host: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::get(uri);
    if let Some(h) = host {
        req = req.header("host", h);
    }
    let res = router(app).oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
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
        held_out: false,
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
        from: None,
        group: None,
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

#[tokio::test]
async fn a_group_of_attempts_shows_with_its_reflection() {
    let base = std::env::temp_dir().join(format!("borg-web-group-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let corp = base.join("corp");
    let now = learn::now_ms();
    seed(&corp, now);
    let (_, v) = get(app(vec![("corp", corp.clone())]), "/api/hive?c=corp").await;
    assert_eq!(v["goals"][0]["groups"], serde_json::json!([]), "no group, no rows");
    assert_eq!(v["goals"][0]["reworks"], serde_json::json!([]), "no rework, no rows");
    let member = |n: usize| Task {
        id: hive::group::member_id("t3", n),
        role: Role::Work,
        title: "write t3.txt".into(),
        repo: String::new(),
        files: vec!["t3.txt".into()],
        deps: vec![],
        accept: vec![],
        of: None,
        not_by: vec![],
        flags: vec![],
        from: None,
        group: Some("t3".into()),
    };
    board::append(&hive::tasks_path(&corp, "g"), &[Event::open(member(1), "engineer:e", now - 3), Event::open(member(2), "engineer:e", now - 3)]).unwrap();
    let (_, v) = get(app(vec![("corp", corp)]), "/api/hive?c=corp").await;
    let row = &v["goals"][0]["groups"][0];
    assert_eq!((row["group"].as_str(), row["members"].as_u64(), row["attempts"].as_u64()), (Some("t3"), Some(2), Some(0)), "{v}");
    assert_eq!((row["reflection"].as_str(), row["integrates"].as_str()), (Some("waiting"), Some("")));
    let _ = std::fs::remove_dir_all(&base);
}

/// A promoted playbook shows with its steps, the belief of each version shown and the edits waiting
/// for the engineer.
#[tokio::test]
async fn a_playbook_shows_its_steps_belief_and_waiting_edits() {
    let base = std::env::temp_dir().join(format!("borg-web-playbook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let corp = base.join("corp");
    seed(&corp, learn::now_ms());
    let (_, v) = get(app(vec![("corp", corp.clone())]), "/api/hive?c=corp").await;
    assert_eq!(v["playbooks"], serde_json::json!([]));
    let pb = borg::playbook::playbook_id("app", "fix the parser");
    let edit = |text: &str, ms: i64| borg::playbook::Edit {
        pb: pb.clone(),
        op: borg::playbook::Op::Add,
        step: borg::playbook::bullet_id(&pb, text),
        text: text.into(),
        shape: "fix the parser".into(),
        repo: "app".into(),
        ms,
        ..Default::default()
    };
    // One edit promoted through the queen, one still waiting for an answer.
    assert!(borg::playbook::propose(&corp, &edit("run the parser tests", 1), None, "s1", "", &[]).unwrap());
    // Before the first promotion the playbook shows with its waiting edit alone.
    let (_, v) = get(app(vec![("corp", corp.clone())]), "/api/hive?c=corp").await;
    let p = &v["playbooks"][0];
    assert_eq!((p["id"].as_str(), p["edits"].as_u64(), p["proposed"].as_u64()), (Some(pb.as_str()), Some(0), Some(1)), "{v}");
    assert_eq!((p["waiting"][0]["op"].as_str(), p["waiting"][0]["text"].as_str()), (Some("add"), Some("run the parser tests")));
    let q = borg::queen::open(&corp).remove(0);
    borg::queen::answer(&corp, &q.id, borg::playbook::PROMOTE_OPTION, "ann@x", 5).unwrap();
    assert!(borg::playbook::propose(&corp, &edit("add a failing test first", 6), None, "s1", "", &[]).unwrap());
    let key = borg::playbook::read(&corp)[0].key();
    borg::playbook::record_shown(&corp, "s2", "app", std::slice::from_ref(&key), 10);
    let vetoed = borg::outcome::Outcome { kind: borg::outcome::VETOED.into(), session: "s2".into(), lessons: vec![key.clone()], ms: 20, ..Default::default() };
    borg::outcome::append(&corp.join("outcomes.jsonl"), &vetoed).unwrap();
    let (_, v) = get(app(vec![("corp", corp)]), "/api/hive?c=corp").await;
    let p = &v["playbooks"][0];
    assert_eq!((p["shape"].as_str(), p["edits"].as_u64(), p["proposed"].as_u64()), (Some("fix the parser"), Some(1), Some(1)), "{v}");
    assert_eq!(p["steps"][0]["text"], "run the parser tests");
    assert_eq!(p["waiting"][0]["text"], "add a failing test first");
    let version = &p["versions"][0];
    assert_eq!((version["key"].as_str(), version["vetoed"].as_u64()), (Some(key.as_str()), Some(1)));
    assert!(version["mean"].as_f64().unwrap() < 0.5);
    let _ = std::fs::remove_dir_all(&base);
}

/// DNS rebinding: a page whose name re-resolves to 127.0.0.1 reads the port as its own origin, so the
/// loopback bind alone does not keep the goals, contracts and spend in. Only a loopback Host is served.
#[tokio::test]
async fn a_page_on_another_host_name_is_refused() {
    let base = std::env::temp_dir().join(format!("borg-web-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    seed(&base.join("corp"), learn::now_ms());
    let app = app(vec![("corp", base.join("corp"))]);
    for host in [Some("evil.example:7979"), Some("evil.example"), Some("127.0.0.1.evil.example:7979"), Some("localhost@evil.example"), None] {
        for uri in ["/api/hive?c=corp", "/api/lessons", "/"] {
            let (status, _) = get_as(app.clone(), uri, host).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{host:?} {uri}");
        }
    }
    for host in ["127.0.0.1:7979", "localhost:7979", "LOCALHOST:1", "[::1]:7979", "localhost"] {
        let (status, v) = get_as(app.clone(), "/api/hive?c=corp", Some(host)).await;
        assert_eq!((status, v["collective"].as_str()), (StatusCode::OK, Some("corp")), "{host}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
