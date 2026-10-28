//! The page's playbooks API over a seeded store, in process (no port): `/api/playbooks` shows a
//! playbook's steps, each shown version's belief and the edits waiting for an engineer, written through
//! playbook.rs's own API; and the page answers only a loopback Host.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
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

/// A promoted playbook shows with its steps, the belief of each version shown and the edits waiting
/// for the engineer.
#[tokio::test]
async fn a_playbook_shows_its_steps_belief_and_waiting_edits() {
    let base = std::env::temp_dir().join(format!("borg-web-playbook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let corp = base.join("corp");
    std::fs::create_dir_all(&corp).unwrap();
    let (_, v) = get(app(vec![("corp", corp.clone())]), "/api/playbooks?c=corp").await;
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
    let (_, v) = get(app(vec![("corp", corp.clone())]), "/api/playbooks?c=corp").await;
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
    let (_, v) = get(app(vec![("corp", corp)]), "/api/playbooks?c=corp").await;
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
/// loopback bind alone does not keep the lessons and playbooks in. Only a loopback Host is served.
#[tokio::test]
async fn a_page_on_another_host_name_is_refused() {
    let base = std::env::temp_dir().join(format!("borg-web-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("corp")).unwrap();
    let app = app(vec![("corp", base.join("corp"))]);
    for host in [Some("evil.example:7979"), Some("evil.example"), Some("127.0.0.1.evil.example:7979"), Some("localhost@evil.example"), None] {
        for uri in ["/api/playbooks?c=corp", "/api/lessons", "/"] {
            let (status, _) = get_as(app.clone(), uri, host).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{host:?} {uri}");
        }
    }
    for host in ["127.0.0.1:7979", "localhost:7979", "LOCALHOST:1", "[::1]:7979", "localhost"] {
        let (status, v) = get_as(app.clone(), "/api/playbooks?c=corp", Some(host)).await;
        assert_eq!((status, v["collective"].as_str()), (StatusCode::OK, Some("corp")), "{host}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
