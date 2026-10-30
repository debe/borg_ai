//! `/api/update` over a seeded store, in process (no port): this borg's version and protocol, the last
//! release check read from the root store's `update.json` (borg update.rs), and the members by
//! version: one on this node's protocol, one on an older one with what it misses (borg protocol.rs
//! `NOTES`), and one nothing syncs with. A store without `mesh/` or a check has none of it, never an error.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg::protocol::{self, Mismatch, PeerVersion};
use borg::update;
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
async fn the_update_view_names_the_release_and_the_members_by_version() {
    let root = std::env::temp_dir().join(format!("borg-web-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // A named collective's store sits under the root, which holds the one release check per machine.
    let home = root.join("collectives").join("corp");
    std::fs::create_dir_all(&home).unwrap();
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Team, mode: "hook" };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), cwd: String::new(), stop });

    // Nothing checked, no mesh: this borg alone.
    let (status, v) = get(app.clone(), "/api/update?c=corp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v,
        json!({"collective": "corp", "borg": update::VERSION, "protocol": protocol::current(), "check": update::on(),
            "latest": null, "checked_ms": null, "error": null, "update": false, "members": []})
    );

    // A newer release, as the check records it (with the field only borg doctor reads).
    let newer = "99.0.0";
    std::fs::write(root.join("update.json"), json!({"checked_ms": 5_000, "latest": newer, "error": null, "failing_since_ms": null}).to_string()).unwrap();
    let ours = protocol::current();
    let older = ours - 1;
    protocol::note_version(&home, "k-same", &PeerVersion { borg: Some(update::VERSION.into()), protocol: ours, ms: 1, engineer: Some("ann@x".into()) })
        .unwrap();
    protocol::note_version(&home, "k-old", &PeerVersion { borg: None, protocol: older, ms: 2, engineer: None }).unwrap();
    // A member that completed a session once, but speaks a newer protocol now.
    protocol::note_version(&home, "k-new", &PeerVersion { borg: Some("0.6.0".into()), protocol: ours, ms: 3, engineer: Some("bob@x".into()) }).unwrap();
    let theirs = format!("borg/mesh/{}", ours + 1);
    protocol::note_mismatch(&home, "k-new", Mismatch { theirs: theirs.clone(), ours: format!("borg/mesh/{ours}"), ms: 4 }).unwrap();

    let (status, v) = get(app.clone(), "/api/update").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((v["latest"].as_str(), v["checked_ms"].as_i64(), &v["error"]), (Some(newer), Some(5_000), &Value::Null), "{v}");
    assert_eq!(v["update"], update::on(), "a newer release is an update unless the check is off: {v}");
    let members = v["members"].as_array().unwrap();
    let keys: Vec<&str> = members.iter().filter_map(|m| m["key"].as_str()).collect();
    assert_eq!(keys, ["k-old", "k-same", "k-new"], "the members that sync by key, then the ones nothing syncs with: {v}");
    let note = protocol::misses(older, ours);
    assert_eq!(
        members[0],
        json!({"key": "k-old", "engineer": null, "name": "peer k-old", "borg": null, "protocol": older, "ms": 2, "syncs": true, "note": note})
    );
    assert_eq!(
        members[1],
        json!({"key": "k-same", "engineer": "ann@x", "name": "ann@x", "borg": update::VERSION, "protocol": ours, "ms": 1, "syncs": true, "note": null})
    );
    let m = &members[2];
    assert_eq!((m["syncs"].as_bool(), m["protocol"].as_u64(), m["name"].as_str()), (Some(false), Some(u64::from(ours + 1)), Some("bob@x")), "{m}");
    assert!(m["note"].as_str().is_some_and(|n| n.contains(&theirs) && n.ends_with("upgrade this node")), "{m}");

    // A broken record reads as none.
    std::fs::write(root.join("update.json"), "{not json").unwrap();
    let (_, v) = get(app.clone(), "/api/update?c=corp").await;
    assert_eq!((&v["latest"], &v["update"]), (&Value::Null, &json!(false)), "{v}");

    let (status, _) = get(app, "/api/update?c=elsewhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&root);
}
