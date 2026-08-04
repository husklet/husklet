//! Rejection of a malformed image archive without state mutation.

use crate::api::support::{require, wait_for_path};
use hl_client::Client;
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let bad_archive = work.path().join("malformed.tar");
    std::fs::write(&bad_archive, b"this is not a tar archive")?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;
    let error = client
        .images()
        .load(tokio::fs::File::open(&bad_archive).await?)
        .await
        .expect_err("malformed image archive unexpectedly loaded");
    match error {
        hl_client::Error::Docker { status, message } => {
            require(
                status.as_u16() == 400,
                &format!("malformed image archive was HTTP {status}: {message}"),
            )?;
            require(!message.is_empty(), "malformed image archive error had no message")?;
        }
        other => {
            return Err(format!("malformed archive returned non-Docker error: {other}").into());
        }
    }
    client.ping().await?;
    require(
        client.images().list().await?.is_empty(),
        "rejected image archive partially mutated metadata",
    )?;
    let _ = shutdown.send(());
    server.await??;
    println!("PASS malformed-image-archive");
    Ok(())
}
