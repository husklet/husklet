//! Container durability across full daemon process restarts.

use crate::api::support::{require, spawn_daemon, wait_for_socket};
use hl_client::Client;
use hl_container::{Config, ContainerSpec, Containers, Process};
use tempfile::TempDir;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let state = work.path().join("state");
    let rootfs = work.path().join("rootfs");
    let socket = work.path().join("daemon.sock");
    std::fs::create_dir(&rootfs)?;
    let identity = Containers::builder(Config::new(&state))
        .build()
        .await?
        .create(
            ContainerSpec::from_directory(&rootfs, Process::new("/bin/true"))
                .name("process-restart"),
        )
        .await?
        .id
        .to_string();

    for restart in 0..2 {
        let mut daemon = spawn_daemon(&state, &socket)?;
        wait_for_socket(&mut daemon, &socket).await?;
        let client = Client::unix(&socket)?;
        let records = client.containers().list(true).await?;
        require(
            records.len() == 1 && records[0].metadata.id == identity,
            "daemon process restart did not preserve container identity",
        )?;
        if restart == 1 {
            client
                .containers()
                .rename("process-restart", "after-process-restart")
                .await?;
            client
                .containers()
                .remove("after-process-restart", false, false)
                .await?;
        }
        daemon.kill().await?;
        let _ = daemon.wait().await?;
    }
    require(
        Containers::builder(Config::new(&state))
            .build()
            .await?
            .list()
            .await?
            .is_empty(),
        "server-side removal was not durable after process exit",
    )?;
    println!("PASS server-restart-persistence");
    Ok(())
}
