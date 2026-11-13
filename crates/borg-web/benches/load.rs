//! What borg-web holds under load: eight requests at once to each `/api` view of a store of a real team's
//! size, round after round, as several open pages refetching at once do.
//!
//! The store is a copy of the one `BORG_BENCH_STORE` names (its files and directories, but `vectors/`
//! and the mesh's key: `mesh/` gives only `origins.jsonl` and `node.pub`), else a generated one of the
//! same shape (borg's benches/shape/mod.rs). One warm-up request per view first (it backfills the derived
//! history once, as a fresh service does) prints a hash of each body, so two builds can be checked to
//! serve the same pages (a view that tells ages or decays by the clock, such as the lessons' activation,
//! differs run to run); then the peak resident set is reset (Linux `clear_refs` 5) and `BORG_BENCH_ROUNDS`
//! rounds (default 20, about 20 s on a team's store) each fire `CONCURRENT` requests at once per view, a
//! view after the other, on as many worker threads. Prints the requests served, the resident set before,
//! its peak during the rounds (`VmHWM`) and what the allocator kept after them (`VmRSS`). Fixed work, no
//! clock: heaptrack of `BORG_BENCH_ROUNDS=0` against `1` gives one round's allocations and heap peak.
//!
//! `/api/events` (a stream that never ends) and `/api/messages/seen` (it writes) are left out.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use borg::collective::Share;
use borg_web::{App, Mind, router};
use tower::ServiceExt;

#[path = "../../borg/benches/shape/mod.rs"]
mod shape;

/// Rounds of requests after the warm-up.
const ROUNDS: &str = "BORG_BENCH_ROUNDS";
/// Requests at once per view: the 8 parallel requests the live service was seen to hold 788 MB for.
const CONCURRENT: usize = 8;
/// Every view the page fetches.
const VIEWS: [&str; 11] = ["collectives", "stats", "lessons", "memory", "drones", "feed", "queen", "messages", "learning", "select", "update"];

fn main() {
    let rounds: usize = std::env::var(ROUNDS).map_or(20, |n| n.trim().parse().unwrap_or_else(|_| panic!("{ROUNDS} is a count of rounds, not {n:?}")));
    let scratch = shape::Scratch::new("web-load");
    let home = scratch.0.join("store");
    match std::env::var_os(shape::ENV) {
        Some(src) => {
            copy_store(Path::new(&src), &home);
            eprintln!("bench store: a copy of {}", Path::new(&src).display());
        }
        None => eprintln!("bench store: {}", shape::fill(&home)),
    }
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(CONCURRENT).enable_all().build().expect("a tokio runtime");
    let (_stopping, stop) = tokio::sync::watch::channel(false);
    let mind = Mind { name: "bench".into(), home: home.clone(), share: Share::Team };
    let app = Arc::new(App { minds: vec![mind], first: "bench".into(), cwd: String::new(), stop });
    rt.block_on(async {
        for view in VIEWS {
            let body = get(app.clone(), view).await;
            let mut h = DefaultHasher::new();
            body.hash(&mut h);
            eprintln!("warm-up /api/{view}: {} bytes, hash {:016x}", body.len(), h.finish());
        }
        let before = resident("VmRSS:");
        let _ = std::fs::write("/proc/self/clear_refs", "5");
        let mut served = 0;
        for _ in 0..rounds {
            for view in VIEWS {
                let at_once: Vec<_> = (0..CONCURRENT).map(|_| tokio::spawn(get(app.clone(), view))).collect();
                for r in at_once {
                    r.await.expect("a request task");
                    served += 1;
                }
            }
        }
        let kb = |r: Option<u64>| r.map_or("?".into(), |k| k.to_string());
        eprintln!(
            "{served} requests ({rounds} rounds x {} views x {CONCURRENT} at once); RSS {} kB before, peak {} kB during, {} kB after",
            VIEWS.len(),
            kb(before),
            kb(resident("VmHWM:")),
            kb(resident("VmRSS:"))
        );
    });
}

/// The body of `/api/<view>` of the one collective; panics on any status but 200.
async fn get(app: Arc<App>, view: &str) -> axum::body::Bytes {
    let req = Request::get(format!("/api/{view}")).header("host", "127.0.0.1:7979").body(Body::empty()).expect("a request");
    let res = router(app).oneshot(req).await.expect("the router answers");
    assert_eq!(res.status(), StatusCode::OK, "/api/{view}");
    axum::body::to_bytes(res.into_body(), usize::MAX).await.expect("the body")
}

/// Copies the store `from` into `to`: every file and directory, but `vectors/` (no view reads it) and of
/// `mesh/` only `origins.jsonl` and `node.pub` (never its key).
fn copy_store(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("bench store dir");
    for e in std::fs::read_dir(from).expect("BORG_BENCH_STORE is a directory").flatten() {
        let (name, path) = (e.file_name(), e.path());
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_file() {
            std::fs::copy(&path, to.join(&name)).expect("copy the bench store");
        } else if kind.is_dir() && name == "mesh" {
            std::fs::create_dir_all(to.join("mesh")).expect("bench store dir");
            for f in ["origins.jsonl", "node.pub"] {
                let _ = std::fs::copy(path.join(f), to.join("mesh").join(f));
            }
        } else if kind.is_dir() && name != "vectors" {
            copy_store(&path, &to.join(&name));
        }
    }
}

/// A field of `/proc/self/status` in kB; Linux only.
fn resident(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| l.strip_prefix(field))?.trim().trim_end_matches("kB").trim().parse().ok()
}
