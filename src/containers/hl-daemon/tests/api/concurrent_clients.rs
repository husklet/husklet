//! Parallel creates and concurrent client reads over the daemon socket.

use crate::api::support::{require, wait_for_path};
use hl_client::Client;
use hl_container::{Config, ContainerSpec, Containers, Process};
use hl_daemon::Daemon;
use tempfile::TempDir;
use tokio::{sync::oneshot, task::JoinSet};

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    const CONTAINERS: usize = 24;
    const READERS: usize = 48;
    let work = TempDir::new()?;
    let rootfs = work.path().join("rootfs");
    std::fs::create_dir(&rootfs)?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let mut creates = JoinSet::new();
    for index in 0..CONTAINERS {
        let service = containers.clone();
        let rootfs = rootfs.clone();
        creates.spawn(async move {
            service
                .create(
                    ContainerSpec::from_directory(rootfs, Process::new("/bin/true")).name(format!("parallel-{index}")),
                )
                .await
                .map(|container| container.id.to_string())
                .map_err(|error| error.to_string())
        });
    }
    let mut identities = Vec::with_capacity(CONTAINERS);
    while let Some(result) = creates.join_next().await {
        identities.push(result.map_err(|error| error.to_string())??);
    }
    identities.sort();
    identities.dedup();
    require(
        identities.len() == CONTAINERS,
        "parallel creates did not produce unique identities",
    )?;

    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;
    let mut readers = JoinSet::new();
    for index in 0..READERS {
        let client = client.clone();
        readers.spawn(async move {
            if index % 2 == 0 {
                client
                    .containers()
                    .list(true)
                    .await
                    .map(|items| items.len())
                    .map_err(|error| error.to_string())
            } else {
                client
                    .containers()
                    .inspect(&format!("parallel-{}", index % CONTAINERS))
                    .await
                    .map(|_| CONTAINERS)
                    .map_err(|error| error.to_string())
            }
        });
    }
    while let Some(result) = readers.join_next().await {
        require(
            result.map_err(|error| error.to_string())?? == CONTAINERS,
            "concurrent client observed incomplete container state",
        )?;
    }
    let _ = shutdown.send(());
    server.await??;
    println!("PASS concurrent-clients");
    Ok(())
}
