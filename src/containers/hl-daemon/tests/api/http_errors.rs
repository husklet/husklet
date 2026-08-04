//! Docker-compatible HTTP error responses over the daemon socket.

use crate::api::support::{raw_http, require, wait_for_path};
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let missing = raw_http(
        &socket,
        b"GET /v1.43/containers/missing/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    require(
        missing.starts_with("HTTP/1.1 404"),
        "missing container was not HTTP 404",
    )?;
    require(missing.contains("\"message\""), "Docker error omitted its JSON message")?;

    let malformed = raw_http(
        &socket,
        b"POST /v1.43/containers/create HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{",
    )
    .await?;
    require(malformed.starts_with("HTTP/1.1 400"), "malformed JSON was not HTTP 400")?;

    let unknown = raw_http(
        &socket,
        b"GET /v1.43/not-a-real-resource HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    require(
        unknown.starts_with("HTTP/1.1 404"),
        "unknown API route was not HTTP 404",
    )?;

    let _ = shutdown.send(());
    server.await??;
    println!("PASS http-errors");
    Ok(())
}
