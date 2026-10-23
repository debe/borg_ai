//! The page's drones API over a seeded store, in process (no port): every visible harness's drone shows
//! in `/api/drones` with its harness and state, its collective in `/api/collectives`. The hook writes
//! these records (presence.rs `touch`); crates/borg/tests/conformance_cli.rs proves that end to end.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg::{harness, learn, presence};
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

/// GET `uri` from the page's router: (status, JSON body).
async fn get(app: Arc<App>, uri: &str) -> (StatusCode, Value) {
    let res = router(app).oneshot(Request::get(uri).header("host", "127.0.0.1:7979").body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn every_harness_drone_shows_with_its_harness_and_state() {
    let home: PathBuf = std::env::temp_dir().join(format!("borg-web-drones-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let now = learn::now_ms();
    let names: Vec<&str> = harness::visible().map(|h| h.name()).collect();
    for (i, name) in names.iter().enumerate() {
        let ev = |extra: Value| {
            let mut v = json!({"session_id": format!("s-{name}"), "cwd": "/work/harbor_orion"});
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            v
        };
        presence::touch(&home, "corp", name, "prompt", &ev(json!({"prompt": format!("Tidy the Orion net {i}")})), now).unwrap();
        presence::touch(&home, "corp", name, "post_tool", &ev(json!({"tool_name": "Edit", "tool_input": {"file_path": "/work/harbor_orion/src/net.rs"}})), now)
            .unwrap();
    }
    // The last one's turn ended, and the drone with it.
    let last = names.last().unwrap();
    presence::touch(&home, "corp", last, "end", &json!({"session_id": format!("s-{last}"), "cwd": "/work/harbor_orion"}), now).unwrap();

    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Local, mode: "live" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), stop });

    let (status, v) = get(app.clone(), "/api/collectives").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["collectives"], json!([{"name": "corp", "share": "local", "mode": "live"}]), "{v}");
    assert_eq!(v["first"], "corp");

    let (status, v) = get(app.clone(), "/api/drones?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["collective"], "corp");
    let drones = v["drones"].as_array().unwrap();
    assert_eq!(drones.len(), names.len(), "{v}");
    for name in &names {
        let d = drones.iter().find(|d| d["session"] == format!("s-{name}")).unwrap_or_else(|| panic!("{name}: {v}"));
        assert_eq!(d["harness"], *name, "{d}");
        assert_eq!(d["state"], if name == last { "ended" } else { "working" }, "{d}");
        assert_eq!(d["files"][0], "src/net.rs", "{d}");
        assert!(d["prompt"].as_str().unwrap().starts_with("Tidy the Orion net"), "{d}");
    }

    let (status, _) = get(app, "/api/drones?c=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_veto_shows_in_the_feed_and_on_the_lessons_it_credited() {
    let home: PathBuf = std::env::temp_dir().join(format!("borg-web-veto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let lesson = json!({"id": "L1", "origin_ms": 1, "question": "Do merge lookups use the review API?", "consequence": "wrong merges"});
    std::fs::write(home.join("lessons.jsonl"), format!("{lesson}\n")).unwrap();
    let asked = borg::outcome::Outcome { kind: borg::outcome::ASKED.into(), session: "s1".into(), lessons: vec!["L1".into()], ms: 2, ..Default::default() };
    borg::outcome::append(&home.join("outcomes.jsonl"), &asked).unwrap();
    let said = borg::veto::Said { session: "s1", repo: "app", engineer: "a@x", task: "t", agent: "", reason: "never the commit log", ms: 3, via: "cli" };
    borg::veto::record(&home, &said).unwrap();

    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Local, mode: "live" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), stop });
    let (status, v) = get(app.clone(), "/api/feed?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    let veto = v["events"].as_array().unwrap().iter().find(|e| e["kind"] == "veto").unwrap_or_else(|| panic!("{v}"));
    assert_eq!((veto["lessons"].clone(), veto["text"].as_str()), (json!(["L1"]), Some("never the commit log")));
    let (_, v) = get(app, "/api/lessons?c=corp").await;
    assert_eq!(v["lessons"][0]["vetoed"], 1, "{v}");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_hacked_hive_task_shows_on_the_lessons_it_credited_and_in_the_feed() {
    let home: PathBuf = std::env::temp_dir().join(format!("borg-web-hacked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let lesson = json!({"id": "L1", "origin_ms": 1, "question": "Is the parser's empty input handled?", "consequence": "a crash"});
    std::fs::write(home.join("lessons.jsonl"), format!("{lesson}\n")).unwrap();
    let hacked = borg::outcome::Outcome {
        kind: borg::outcome::HACKED.into(),
        session: "s1".into(),
        lessons: vec!["L1".into()],
        ms: 2,
        cause: "g/t1.v1".into(),
        ..Default::default()
    };
    borg::outcome::append(&home.join("outcomes.jsonl"), &hacked).unwrap();
    let mut ev = borg::stats::Event::new(borg::stats::TASK_HACKED, 3);
    ev.session = "s1".into();
    ev.lessons = vec!["L1".into()];
    borg::stats::record(&home, &ev);

    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Local, mode: "live" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), stop });
    let (_, v) = get(app.clone(), "/api/lessons?c=corp").await;
    assert_eq!((v["lessons"][0]["hacked"].clone(), v["lessons"][0]["verified"].clone()), (json!(1), json!(0)), "{v}");
    let (_, v) = get(app, "/api/feed?c=corp").await;
    assert!(v["events"].as_array().unwrap().iter().any(|e| e["kind"] == "task_hacked"), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}
