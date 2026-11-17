//! The page's hand-offs API over a seeded store, in process (no port): an offer asking an engineer shows
//! as asked, with the engineers whose machine could take it (a clone of its repo, speaking hand-off), and
//! the machines the presence holds; a machine without the clone, or on a borg without hand-offs, is no
//! taker.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg::presence::{self, Cloned, Machine, Presence, Remote};
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn get(app: Arc<App>, uri: &str) -> (StatusCode, Value) {
    let res = router(app).oneshot(Request::get(uri).header("host", "127.0.0.1:7979").body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// A remote drone's record with its machine's record (signatures are checked on receipt, not here).
fn remote(key: &str, engineer: &str, now: i64, machine: Option<Machine<'_>>) -> String {
    let p = Presence {
        session: format!("s-{engineer}"),
        harness: "claude".into(),
        engineer: engineer.into(),
        repo: "app".into(),
        collective: "corp".into(),
        prompt: String::new(),
        files: vec![],
        event: "prompt".into(),
        ms: now,
        started: now,
        pid: None,
    };
    let machine = machine.map(|m| serde_json::to_string(&m).unwrap());
    serde_json::to_string(&Remote { p, key: key.into(), sig: String::new(), machine, msig: String::new() }).unwrap()
}

#[tokio::test]
async fn an_ask_shows_with_who_could_take_it() {
    let home: PathBuf = std::env::temp_dir().join(format!("borg-web-handoffs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("mesh")).unwrap();
    let now = borg::learn::now_ms();
    let (node, nonce) = ("a".repeat(64), "0011223344556677");
    let mut h = blake3::Hasher::new();
    for part in [node.as_str(), "s1", nonce] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    let id = format!("h{}", &h.finalize().to_hex()[..12]);
    let sha = "1".repeat(40);
    let offer = json!({"v": 1, "id": id, "kind": "offer", "node": node, "session": "s1", "engineer": "alice@x", "harness": "claude", "to": "bob@y",
        "nonce": nonce, "ms": now - 60_000, "repos": [{"name": "app", "remote": "/srv/app.git", "branch": "main", "head": sha, "state": sha, "tree": sha}],
        "note": "next: tests for b", "brief": sha});
    std::fs::write(home.join("handoffs.jsonl"), format!("{offer}\n")).unwrap();
    // A clone list names a repo by blake3 of its normalized URL (a local path, without its `.git`).
    let app = blake3::hash(b"/srv/app").to_hex()[..32].to_string();
    let with = |url: &str, handoff: u8| {
        Some(Machine { borg: "x".into(), handoff, clones: vec![Cloned { url: url.to_string().into(), name: "app".into() }], ..Machine::default() })
    };
    let lines = [
        remote(&"b".repeat(64), "bob@y", now, with(&app, 1)),
        remote(&"c".repeat(64), "carol@z", now, with(&"0".repeat(32), 1)),
        remote(&"d".repeat(64), "dave@w", now, with(&app, 0)),
        remote(&"e".repeat(64), "erin@v", now, None),
    ];
    std::fs::write(presence::remote_path(&home), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Team };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), cwd: String::new(), stop });

    let (status, v) = get(app.clone(), "/api/handoffs?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    let h = &v["handoffs"][0];
    assert_eq!((h["id"].as_str(), h["state"].as_str(), h["to"].as_str()), (Some(id.as_str()), Some("asked"), Some("bob@y")), "{v}");
    assert_eq!(h["takers"], json!(["bob@y"]), "only bob has the clone and speaks hand-off: {v}");
    assert_eq!((h["repos"].clone(), h["by"].clone()), (json!(["app"]), Value::Null), "{v}");
    let machines = v["machines"].as_array().unwrap();
    assert_eq!(machines.len(), 4, "{v}");
    let erin = machines.iter().find(|m| m["engineers"] == json!(["erin@v"])).unwrap();
    assert_eq!((erin["handoff"].as_u64(), erin["borg"].is_null()), (Some(0), true), "a borg without hand-offs: {erin}");
    let (status, _) = get(app, "/api/handoffs?c=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&home);
}
