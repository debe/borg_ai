//! `/api/queen` carries the queen's attention reward (borg escalation.rs), in process (no port): folded
//! from this node's stat lines and a peer's synced rollup alike.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn the_queen_view_shows_the_teams_attention_reward() {
    let home = std::env::temp_dir().join(format!("borg-web-queen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("stats")).unwrap();
    let ms = borg::learn::now_ms();
    let here = [
        json!({"ms": ms, "kind": "queen_no", "session": "s1", "why": "PublicApi", "text": "q1 keep the old contract"}),
        json!({"ms": ms, "kind": "queen_stamp", "session": "s2", "why": "Irreversible", "text": "q2 proceed as planned"}),
    ];
    std::fs::write(borg::stats::events_path(&home), here.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    // A peer's rollup: one more "no" and one missed escalation.
    let peer = json!({"id": "peer/b@x/2026-10-24", "version": 1, "day": "2026-10-24", "node": "peer", "engineer": "b@x",
        "counts": {"queen_no": 1, "queen_missed": 1}, "usd": 0.0, "dur_ms": 0});
    std::fs::write(home.join("stats.jsonl"), format!("{peer}\n")).unwrap();
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Team, mode: "hook" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), cwd: String::new(), stop });
    let res = router(app).oneshot(Request::get("/api/queen?c=corp").header("host", "127.0.0.1:7979").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    let a = &v["attention"];
    assert_eq!((a["answered"].as_u64(), a["no"].as_u64(), a["stamps"].as_u64(), a["missed"].as_u64()), (Some(3), Some(2), Some(1), Some(1)), "{v}");
    assert_eq!(a["share"].as_f64(), Some(2.0 / 3.0));
    assert_eq!(a["reward"].as_f64(), Some(2.0 - 1.0 - 3.0));
    let _ = std::fs::remove_dir_all(&home);
}
