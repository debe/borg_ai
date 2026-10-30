//! `/api/memory` over a seeded store, in process (no port): the collective's evidence records exactly as
//! `borg memory export --json` prints them, an absorbed lesson with its note, absorb time and recurrence,
//! and `absorbed=true` keeping the absorbed ones only.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg::learn::Lesson;
use borg::memory_export::{self, Filter};
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn get(app: Arc<App>, uri: &str) -> (StatusCode, Value) {
    let res = router(app).oneshot(Request::get(uri).header("host", "127.0.0.1:7979").body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_memory_view_is_the_export_with_absorbs_and_recurrence() {
    let home = std::env::temp_dir().join(format!("borg-web-memory-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let lesson = |id: &str, version: u32, episodes: &[&str]| Lesson {
        id: id.into(),
        origin_ms: 1_000,
        question: format!("question {id}?"),
        episodes: episodes.iter().map(|e| e.to_string()).collect(),
        version,
        ..Default::default()
    };
    // L1 was absorbed at version 1; a repeated correction merged one more episode into it since (version 2).
    let lines = [lesson("L1", 1, &["e-1"]), lesson("L2", 1, &["e-2"]), lesson("L1", 2, &["e-1", "e-3"])];
    std::fs::write(home.join("lessons.jsonl"), lines.iter().map(|l| serde_json::to_string(l).unwrap() + "\n").collect::<String>()).unwrap();
    let absorb = json!({"id": "L1", "kind": "absorb", "ms": 5_000, "by": "a@x", "note": "skills/review@2026-10-30", "version": 1});
    std::fs::write(borg::absorb::path(&home), format!("{absorb}\n")).unwrap();

    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Team, mode: "hook" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), cwd: String::new(), stop });

    let (status, v) = get(app.clone(), "/api/memory?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, memory_export::document("corp", &home, &Filter::default()), "the page serves the export as the CLI prints it");
    assert_eq!((v["schema"].as_u64(), v["collective"].as_str()), (Some(u64::from(memory_export::SCHEMA)), Some("corp")));
    let ids: Vec<&str> = v["lessons"].as_array().unwrap().iter().filter_map(|l| l["id"].as_str()).collect();
    assert_eq!(ids, ["L1", "L2"], "{v}");
    let l1 = &v["lessons"][0];
    assert_eq!(l1["absorbed"], json!({"note": "skills/review@2026-10-30", "ms": 5_000, "by": "a@x", "version": 1}), "{v}");
    assert_eq!(l1["recurrence"], 1, "one episode merged since the absorbed version: {v}");
    assert_eq!(v["lessons"][1]["absorbed"], Value::Null);

    // The page's filter, as `--absorbed`; the collective the page opens with when it names none.
    let (status, only) = get(app.clone(), "/api/memory?absorbed=true").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = only["lessons"].as_array().unwrap().iter().filter_map(|l| l["id"].as_str()).collect();
    assert_eq!((only["collective"].as_str(), ids), (Some("corp"), vec!["L1"]), "{only}");

    // A malformed log is no absorb, never an error.
    std::fs::write(borg::absorb::path(&home), "{not json\n").unwrap();
    let (status, v) = get(app.clone(), "/api/memory?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    assert!(v["lessons"].as_array().unwrap().iter().all(|l| l["absorbed"].is_null()), "{v}");

    let (status, _) = get(app, "/api/memory?c=elsewhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&home);
}
