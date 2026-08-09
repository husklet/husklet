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
    require(
        unknown.contains("\"message\":\"page not found\""),
        "unknown API route omitted Docker's page-not-found body",
    )?;

    let too_new = raw_http(
        &socket,
        b"GET /v1.51/containers/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    require(
        too_new.starts_with("HTTP/1.1 400"),
        "too-new API version was not HTTP 400",
    )?;
    require(
        too_new.contains("client version 1.51 is too new. Maximum supported API version is 1.43"),
        "too-new API version omitted Docker's refusal message",
    )?;

    let too_old = raw_http(
        &socket,
        b"GET /v1.23/containers/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    require(
        too_old.starts_with("HTTP/1.1 400"),
        "too-old API version was not HTTP 400",
    )?;
    require(
        too_old.contains("client version 1.23 is too old. Minimum supported API version is 1.24"),
        "too-old API version omitted Docker's refusal message",
    )?;

    let _ = shutdown.send(());
    server.await??;
    println!("PASS http-errors");
    Ok(())
}
