//! Standalone daemon process bring-up and version handshake.

use crate::api::support::{require, spawn_daemon, wait_for_socket};
use hl_client::Client;
use tempfile::TempDir;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let socket = work.path().join("daemon.sock");
    let mut child = spawn_daemon(&work.path().join("state"), &socket)?;

    wait_for_socket(&mut child, &socket).await?;
    let client = Client::unix(&socket)?;
    client.ping().await?;
    let version = client.version().await?;
    require(
        !version.api_version.is_empty(),
        "daemon version omitted API version",
    )?;
    require(
        client.containers().list(true).await?.is_empty(),
        "fresh server was not empty",
    )?;
    child.kill().await?;
    let _ = child.wait().await?;
    println!("PASS server-process");
    Ok(())
}
