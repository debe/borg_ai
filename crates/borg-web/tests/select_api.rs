//! `/api/select` serves the collective's lesson selection exactly as borg's monitor reads it (top.rs
//! `Selection`, from the store's selection summary), in process (no port): never recomputed by the
//! page's server, the collective the page picked, and no budget or price unless an engineer set one.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn get(app: Arc<App>, url: &str) -> (StatusCode, Value) {
    let res = router(app).oneshot(Request::get(url).header("host", "127.0.0.1:7979").body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_selection_is_the_monitors_own_reading_of_the_summary() {
    let root = std::env::temp_dir().join(format!("borg-web-select-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (corp, mine) = (root.join("corp"), root.join("mine"));
    for home in [&corp, &mine] {
        std::fs::create_dir_all(home).unwrap();
        std::fs::write(home.join("lessons.jsonl"), "").unwrap();
    }
    // Two graded checks of one session, the second asking its 3, and a third whose learned decision the
    // fixed rule took over (another check held the decision lock): what the summary folds.
    let lines = [
        json!({"kind": "graded", "session": "s", "lessons": ["A", "B"], "grades": [2, 1], "applied": 2, "channel": "live", "ms": 1, "repo": "r",
            "policy": "learned", "check": "0123456789ab", "free": ["A", "B"], "pf": [1.0, 1.0]}),
        json!({"kind": "graded", "session": "s", "lessons": ["A"], "grades": [3], "applied": 1, "chosen": ["A"], "p": [1.0], "channel": "stop", "ms": 2,
            "repo": "r", "policy": "learned", "check": "ba9876543210"}),
        json!({"kind": "graded", "session": "s", "lessons": ["C"], "grades": [3], "applied": 1, "chosen": ["C"], "p": [1.0], "channel": "live", "ms": 3,
            "repo": "r", "policy": "fixed", "reason": "lock_busy", "check": "a1b2c3d4e5f6"}),
    ];
    std::fs::write(corp.join("outcomes.jsonl"), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = |name: &str, home: &std::path::Path| Mind { name: name.into(), home: home.to_path_buf(), share: Share::Local, mode: "hook" };
    let app = Arc::new(App { minds: vec![mind("corp", &corp), mind("mine", &mine)], first: "mine".into(), cwd: String::new(), stop });

    let (status, v) = get(app.clone(), "/api/select?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["collective"], "corp");
    // The monitor's own reading of corp's summary, under this process's policy: nothing recomputed.
    let lessons = corp.join("lessons.jsonl");
    let want = borg::top::selection(&lessons, borg::select::policy());
    assert_eq!(v["selection"], serde_json::to_value(&want).unwrap(), "{v}");
    let summary = borg::select::summary::load(&lessons);
    assert_eq!((v["selection"]["at"].as_u64(), v["selection"]["fit"].as_str()), (Some(summary.at), Some(summary.fit.key.as_str())));
    assert_eq!(v["selection"]["policy"], borg::select::policy().name());
    // Each reason a learned decision fell back to the fixed rule, of the graded checks folded.
    let rows = json!([
        {"name": "lock_busy", "part": 1, "whole": 3},
        {"name": "lock_failed", "part": 0, "whole": 3},
        {"name": "unrecorded", "part": 0, "whole": 3},
    ]);
    assert_eq!(v["selection"]["fallbacks"], rows, "{v}");
    // No engineer set a budget: spend is measured, never priced.
    assert_eq!(v["selection"]["budget"], Value::Null, "{v}");

    // Without `c`, the collective the page opens with; an unknown one is not found.
    let (status, v) = get(app.clone(), "/api/select").await;
    assert_eq!((status, &v["collective"]), (StatusCode::OK, &json!("mine")));
    assert_eq!(get(app, "/api/select?c=nope").await.0, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&root);
}
