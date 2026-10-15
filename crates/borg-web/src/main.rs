//! `borg web [--port 7979] [--dir DIR]`: the command line around the page (lib.rs).

use std::path::PathBuf;
use std::sync::Arc;

use borg_web::{App, USAGE, minds, publish, router};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let port: u16 = match flag("--port").map(|p| p.parse()) {
        None => 7979,
        Some(Ok(p)) => p,
        Some(Err(_)) => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let dir = flag("--dir").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let dir = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
    let (minds, first) = match minds(&dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("borg web: invalid collectives config: {e}");
            std::process::exit(1);
        }
    };
    if minds.is_empty() {
        eprintln!("borg web: no collective on this machine");
        std::process::exit(1);
    }
    tokio::spawn(publish(minds.clone()));
    // Loopback only: the page shows lessons and who works where, never to the network.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("borg web: cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!("borg web: http://{addr}/  ({} collectives, showing {first}; Ctrl-C stops)", minds.len());
    let (stopping, stop) = tokio::sync::watch::channel(false);
    let app = Arc::new(App { minds, first, stop });
    let stop = async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stopping.send(true);
    };
    if let Err(e) = axum::serve(listener, router(app)).with_graceful_shutdown(stop).await {
        eprintln!("borg web: {e}");
        std::process::exit(1);
    }
}
