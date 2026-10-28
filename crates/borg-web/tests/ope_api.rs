//! `/api/stats` carries `borg debug ope`'s support from the local outcome log, in process (no port): logged
//! Thompson decisions with a pool, asked lessons without p, and asked records with p but no pool.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn stats_show_the_off_policy_support() {
    let home = std::env::temp_dir().join(format!("borg-web-ope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let lines = [
        json!({"kind": "asked", "session": "a", "lessons": ["A", "B"], "ms": 1, "repo": "r"}),
        json!({"kind": "asked", "session": "b", "lessons": ["A"], "ms": 2, "repo": "r", "p": [1.0]}),
        json!({"kind": "asked", "session": "c", "lessons": ["A"], "ms": 3, "repo": "r", "p": [0.5], "pool": ["A", "B"], "base": [1.0, 1.0], "scoped": [false, false]}),
        json!({"kind": "asked_then_changed", "session": "c", "lessons": ["A"], "ms": 4, "repo": "r"}),
    ];
    std::fs::write(home.join("outcomes.jsonl"), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Local, mode: "hook" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), stop });
    let res = router(app).oneshot(Request::get("/api/stats?c=corp").header("host", "127.0.0.1:7979").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(v["ope"], json!({"decisions": 1, "no_p": 2, "no_pool": 1}), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}
