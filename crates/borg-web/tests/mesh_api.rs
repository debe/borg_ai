//! The Cube's API over a seeded store of two members, in process (no port): `/api/mesh` gives both
//! machines, this node's link to the other, the local drone, and every kind of event of the window in
//! time order, with their strings named once in `strs`: messages to all and to a session, who was shown
//! one, a hand-off offered and taken, a lesson and its new version by the machines that stamped them,
//! a lesson not stamped yet, an absorb, a sync and a failed one. `since` (the `now` of the response
//! before) gives what arrived after it, though its writer wrote it earlier, and the machines of the whole
//! window. An unknown collective is a 404.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg::msg::{self, Message, Seen, Sender};
use borg::{learn, presence};
use borg_web::{App, Mind, router};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn get(app: Arc<App>, uri: &str) -> (StatusCode, Value) {
    let req = Request::get(uri).header("host", "127.0.0.1:7979").body(Body::empty()).unwrap();
    let res = router(app).oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn lines(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        lines.iter().fold(String::new(), |mut s, l| {
            let _ = writeln!(s, "{l}");
            s
        }),
    )
    .unwrap();
}

fn hex(h: [u8; 32]) -> String {
    h.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The origin record of `line`: stamped by `key` at `ms` (its signature is not read here).
fn origin(line: &str, key: &str, ms: i64) -> String {
    json!({"h": hex(borg::mesh::line_hash(line)), "key": key, "ms": ms, "sig": "00"}).to_string()
}

fn lesson(id: &str, version: u32, origin_ms: i64, question: &str, reach: &str) -> String {
    let mut l = json!({"id": id, "origin_ms": origin_ms, "repo": "app", "question": question, "situations": ["s"], "consequence": "c", "support": 1,
        "episodes": ["E1"], "history": [{"question": "older", "version": 1}], "version": version, "engineers": ["ann@x"], "area": "other"});
    if !reach.is_empty() {
        l["reach"] = json!(reach);
    }
    l.to_string()
}

/// A hand-off line of `node`'s `session`: an offer to `to` (borg handoff/log.rs `Event`, its id bound to
/// its node, session and nonce), or `kind`, an outcome of offer `id`.
fn handoff(node: &str, session: &str, ms: i64, kind: &str, id: &str, to: &str) -> String {
    let mut event = json!({"v": 1, "id": id, "kind": kind, "node": node, "session": session, "ms": ms});
    if kind == "offer" {
        let repo = json!({"name": "app", "remote": "/srv/app", "branch": "main", "head": "1".repeat(40), "state": "2".repeat(40), "tree": "3".repeat(40)});
        event.as_object_mut().unwrap().extend([
            ("to".into(), json!(to)),
            ("nonce".into(), json!("n1")),
            ("repos".into(), json!([repo])),
            ("brief".into(), json!("4".repeat(40))),
            ("note".into(), json!("fix the parser")),
        ]);
    } else {
        event.as_object_mut().unwrap().extend([("claim".into(), json!("5".repeat(40))), ("state".into(), json!("2".repeat(40)))]);
    }
    json!({"v": 1, "kind": "handoff", "node": node, "ms": ms, "event": event}).to_string()
}

/// The id an offer of `node`'s `session` with `nonce` has (borg handoff/mod.rs `id_for`).
fn offer_id(node: &str, session: &str, nonce: &str) -> String {
    let mut h = blake3::Hasher::new();
    for part in [node, session, nonce] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    format!("h{}", &h.finalize().to_hex()[..12])
}

/// What `seed` wrote that the test looks for.
struct Seeded {
    msgs: Vec<String>,
    to_all: String,
    to_ann: String,
    offer: String,
}

/// A store of this node `a` and its peer `b` at `now`, holding every kind of event.
fn seed(home: &Path, a: &str, b: &str, now: i64) -> Seeded {
    let at = |ago: i64| now - ago;
    std::fs::create_dir_all(home.join("mesh")).unwrap();
    std::fs::write(home.join("mesh").join("node.pub"), format!("{a}\n")).unwrap();

    // The messages: one to all from this node's engineer, one from bob's session to ann's, ann's
    // session shown it, an offer of ann's to bob's machine that bob takes, and one past the window.
    let engineer = Sender { by: msg::ENGINEER.into(), name: "Ann".into(), harness: "terminal".into(), node: a.into() };
    let bob = Sender { by: "s-b".into(), name: "Bob - app - claude".into(), harness: "claude".into(), node: b.into() };
    let to_all = Message::new(msg::TELL, &engineer, msg::ALL, None, at(9000), "standup at ten");
    let to_ann = Message::new(msg::ASK, &bob, &format!("s-a@{a}"), None, at(8000), "who owns the lock?");
    let old = Message::new(msg::TELL, &engineer, msg::ALL, None, now - msg::TTL_MS - 60_000, "gone");
    let seen = Seen::new("s-a", a, at(7000), &[&to_ann.id]);
    let h = offer_id(a, "s-a", "n1");
    let msgs = [
        serde_json::to_string(&old).unwrap(),
        serde_json::to_string(&to_all).unwrap(),
        serde_json::to_string(&to_ann).unwrap(),
        serde_json::to_string(&seen).unwrap(),
        handoff(a, "s-a", at(5000), "offer", &h, b),
        handoff(b, "s-b", at(4000), "taken", &h, ""),
    ];
    lines(&msg::path(home), &msgs);

    // The lessons: L1 stamped by bob, its second version by this node, L2 of this node not stamped yet,
    // L0 stamped before the window, and a tombstone, which withdraws a lesson and writes none.
    let l0 = lesson("L0", 1, now - 30 * 86_400_000, "old?", "");
    let l1 = lesson("L1", 1, at(6100), "Is the retry idempotent?\nmore", "");
    let l1b = lesson("L1", 2, at(6100), "Is every retry idempotent?", "");
    let l2 = lesson("L2", 1, at(2500), "Who closes the file?", "");
    let gone = lesson("L3", 2, at(2400), borg::reach::PERSONAL_TOMBSTONE, borg::reach::PERSONAL);
    lines(&home.join("lessons.jsonl"), &[l0.clone(), l1.clone(), l1b.clone(), l2, gone.clone()]);
    let absorbed = json!({"id": "L1", "kind": "absorb", "ms": at(2000), "by": "ann@x", "note": "skills/review@2026-11-20", "version": 2}).to_string();
    lines(&borg::absorb::path(home), std::slice::from_ref(&absorbed));
    let origins = [
        origin(&l0, b, now - 30 * 86_400_000),
        origin(&l1, b, at(6000)),
        origin(&l1b, a, at(3000)),
        origin(&gone, a, at(2400)),
        origin(&absorbed, b, at(2000)),
        origin(&msgs[2], b, at(8000)),
    ];
    lines(&borg::mesh::origins_path(home), &origins);

    // This node's sessions with bob: one that moved five lines, then one that failed.
    let moved = borg::mesh::Session { ms: at(1500), peer: b.into(), dial: true, sent: 2, received: 3, error: None };
    borg::mesh::note(home, &moved, now).unwrap();
    let failed = borg::mesh::Session { ms: at(1000), sent: 0, received: 0, error: Some("no answer within 30s".into()), ..moved };
    borg::mesh::note(home, &failed, now).unwrap();
    std::fs::write(presence::synced_path(home), json!({ b: at(1500) }).to_string()).unwrap();
    std::fs::write(home.join("mesh").join("peers.json"), json!({ b: "127.0.0.1:4433" }).to_string()).unwrap();
    // Ann's drone on this machine.
    presence::touch(home, "corp", "claude", "prompt", &json!({"session_id": "s-a", "cwd": "/work/app", "prompt": "fix the parser"}), now).unwrap();
    Seeded { to_all: to_all.id, to_ann: to_ann.id, offer: h, msgs: msgs.into() }
}

#[tokio::test]
async fn the_cube_shows_both_machines_and_everything_that_moved_between_them() {
    let home: PathBuf = std::env::temp_dir().join(format!("borg-web-mesh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let (ka, kb) = ("a".repeat(64), "b".repeat(64));
    let now = learn::now_ms();
    let at = |ago: i64| now - ago;
    let Seeded { msgs, to_all, to_ann, offer: h } = seed(&home, &ka, &kb, now);

    let (_tx, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "corp".into(), home: home.clone(), share: Share::Team };
    let app = Arc::new(App { minds: vec![mind], first: "corp".into(), cwd: String::new(), stop });

    let (status, _) = get(app.clone(), "/api/mesh?c=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, v) = get(app.clone(), "/api/mesh?c=corp").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["collective"], "corp");
    let strs: Vec<&str> = v["strs"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    let s = |i: &Value| strs[usize::try_from(i.as_u64().unwrap_or_else(|| panic!("an index: {i}"))).unwrap()];
    assert_eq!(s(&v["me"]), ka);
    let mut dedup = strs.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(dedup.len(), strs.len(), "every string once: {strs:?}");

    let nodes = v["nodes"].as_array().unwrap();
    assert_eq!(nodes.iter().map(|n| (s(&n["key"]), n["me"].as_bool().unwrap())).collect::<Vec<_>>(), [(ka.as_str(), true), (kb.as_str(), false)], "{v}");
    let links = v["links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{v}");
    assert_eq!((s(&links[0]["a"]), s(&links[0]["b"]), &links[0]["dial"], &links[0]["ms"]), (ka.as_str(), kb.as_str(), &json!(true), &json!(at(1500))));
    let drones = v["drones"].as_array().unwrap();
    assert_eq!(drones.iter().map(|d| (s(&d["session"]), s(&d["node"]))).collect::<Vec<_>>(), [("s-a", ka.as_str())], "{v}");

    let events = v["events"].as_array().unwrap();
    let row = |e: &Value| {
        let opt = |k: &str| e.get(k).filter(|x| !x.is_null()).map(|x| s(x).to_string());
        (e["kind"].as_str().unwrap().to_string(), e["id"].as_str().unwrap().to_string(), s(&e["from_n"]).to_string(), opt("from_s"), opt("to_n"), opt("to_s"))
    };
    let (sa, sb) = (Some("s-a".to_string()), Some("s-b".to_string()));
    let want = [
        ("tell", to_all.clone(), &ka, None, None, None),
        ("ask", to_ann.clone(), &kb, sb.clone(), Some(ka.clone()), sa.clone()),
        ("seen", "s-a".to_string(), &ka, sa.clone(), None, None),
        ("lesson", "L1@1".to_string(), &kb, None, None, None),
        ("handoff", format!("{h}:offer"), &ka, sa.clone(), Some(kb.clone()), None),
        ("handoff", format!("{h}:taken"), &kb, sb, Some(ka.clone()), None),
        ("lesson_update", "L1@2".to_string(), &ka, None, None, None),
        ("lesson", "L2@1".to_string(), &ka, None, None, None),
        ("absorb", format!("L1@{}", at(2000)), &kb, None, None, None),
        ("sync", format!("sync:{kb}@{}", at(1500)), &ka, None, Some(kb.clone()), None),
        ("sync_failed", format!("sync:{kb}@{}", at(1000)), &ka, None, Some(kb.clone()), None),
    ];
    let got: Vec<_> = events.iter().map(row).collect();
    let want: Vec<_> = want.into_iter().map(|(k, id, f, fs, tn, ts)| (k.to_string(), id, f.clone(), fs, tn, ts)).collect();
    assert_eq!(got, want, "{v}");
    let ms: Vec<i64> = events.iter().map(|e| e["ms"].as_i64().unwrap()).collect();
    assert!(ms.is_sorted(), "{ms:?}");

    // What each kind carries besides.
    let by = |id: &str| events.iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("{id}: {v}"));
    assert_eq!((&by(&to_all)["to"], &by(&to_all)["text"]), (&json!("all"), &json!("standup at ten")));
    assert_eq!(by(&to_ann)["to"], "session");
    assert_eq!(by("s-a")["re"], json!(to_ann));
    assert_eq!((&by("L1@1")["lesson"], &by("L1@1")["text"], &by("L1@1")["ms"]), (&json!("L1"), &json!("Is the retry idempotent?"), &json!(at(6000))));
    assert_eq!(by("L2@1")["ms"], at(2500), "a line not stamped yet is this node's, written with its first version");
    let offer = by(&format!("{h}:offer"));
    assert_eq!((&offer["why"], &offer["text"]), (&json!("offer"), &json!("fix the parser")));
    assert_eq!(by(&format!("{h}:taken"))["why"], "taken");
    let absorb = by(&format!("L1@{}", at(2000)));
    assert_eq!((&absorb["lesson"], &absorb["text"]), (&json!("L1"), &json!("skills/review@2026-11-20")));
    let synced = by(&format!("sync:{kb}@{}", at(1500)));
    assert_eq!((&synced["n"], synced.get("text")), (&json!(5), None));
    let failed = by(&format!("sync:{kb}@{}", at(1000)));
    assert_eq!((failed.get("n"), &failed["text"]), (None, &json!("no answer within 30s")));

    since_gives_what_arrived_after(&app, &home, &v, &msgs, now).await;
    let _ = std::fs::remove_dir_all(&home);
}

/// A tell machine `c` wrote before everything else, synced in after the response `full`: the next
/// request with `since` gives that one alone though it is older than every other, and the strings it
/// and the topology name; `c`, which only that event names, stays a machine of the next delta, which
/// holds no event.
async fn since_gives_what_arrived_after(app: &Arc<App>, home: &Path, full: &Value, msgs: &[String], now: i64) {
    let kc = "c".repeat(64);
    let carol = Sender { by: "s-c".into(), name: "Carol - app - claude".into(), harness: "claude".into(), node: kc.clone() };
    let late = Message::new(msg::TELL, &carol, msg::ALL, None, now - 9500, "from before the partition");
    let mut held = msgs.to_vec();
    held.push(serde_json::to_string(&late).unwrap());
    // Requests a millisecond apart or more, as a page's are: each later one stamps what is new later.
    std::thread::sleep(std::time::Duration::from_millis(2));
    lines(&msg::path(home), &held);
    let (_, later) = get(app.clone(), &format!("/api/mesh?c=corp&since={}", full["now"])).await;
    let ids: Vec<&str> = later["events"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [late.id.as_str()], "{later}");
    assert_eq!(later["from"], later["now"].as_i64().unwrap() - msg::TTL_MS);
    let strs = |v: &Value| v["strs"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(!strs(&later).iter().any(|x| x == "s-b"), "no string only an earlier event named: {later}");
    let keys = |v: &Value| {
        let t = strs(v);
        let mut k: Vec<String> = v["nodes"].as_array().unwrap().iter().map(|n| t[usize::try_from(n["key"].as_u64().unwrap()).unwrap()].clone()).collect();
        k.sort();
        k
    };
    let all = ["a".repeat(64), "b".repeat(64), kc];
    assert_eq!(keys(&later), all, "{later}");
    std::thread::sleep(std::time::Duration::from_millis(2));
    let (_, idle) = get(app.clone(), &format!("/api/mesh?c=corp&since={}", later["now"])).await;
    assert_eq!(idle["events"].as_array().map(Vec::len), Some(0), "{idle}");
    assert_eq!(keys(&idle), all, "a machine an older event names stays: {idle}");
}
