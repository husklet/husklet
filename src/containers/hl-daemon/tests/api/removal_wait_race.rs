//! Race between forced removal and a pending removal waiter.

use crate::api::support::{require, TIMEOUT};
use hl_container::{Config, ContainerSpec, Containers, Process, WaitCondition};
use tempfile::TempDir;
use tokio::time::timeout;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    const REPETITIONS: usize = 32;
    let work = TempDir::new()?;
    let rootfs = work.path().join("rootfs");
    std::fs::create_dir(&rootfs)?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    for index in 0..REPETITIONS {
        let name = format!("race-{index}");
        containers
            .create(ContainerSpec::from_directory(&rootfs, Process::new("/bin/true")).name(&name))
            .await?;
        let waiter = {
            let service = containers.clone();
            let name = name.clone();
            tokio::spawn(async move { service.wait_for(&name, WaitCondition::Removed).await })
        };
        tokio::task::yield_now().await;
        containers.remove_force(&name).await?;
        require(
            timeout(TIMEOUT, waiter).await???.is_none(),
            "removed waiter returned an exit status",
        )?;
    }
    require(
        containers.list().await?.is_empty(),
        "remove/wait race leaked container records",
    )?;
    println!("PASS removal-wait-race");
    Ok(())
}
