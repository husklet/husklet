//! Headless service container lifecycle (create/list/rename/remove).

use crate::api::support::require;
use hl_container::{Config, ContainerSpec, Containers, Process};
use hl_daemon::Daemon;
use tempfile::TempDir;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let state = TempDir::new()?;
    let rootfs = state.path().join("rootfs");
    std::fs::create_dir(&rootfs)?;
    let containers = Containers::builder(Config::new(state.path().join("state")))
        .build()
        .await?;
    let daemon = Daemon::new(containers);
    let service = daemon.headless();
    let created = service
        .containers()
        .create(ContainerSpec::from_directory(&rootfs, Process::new("/bin/true")).name("scenario"))
        .await?;

    let listed = service.containers().list().await?;
    require(listed.len() == 1, "headless list did not return exactly one container")?;
    require(listed[0].id == created.id, "headless list returned the wrong identity")?;
    require(
        service.containers().inspect("scenario").await?.id == created.id,
        "headless name lookup returned the wrong identity",
    )?;
    service.containers().rename("scenario", "renamed").await?;
    require(
        service.containers().inspect("renamed").await?.id == created.id,
        "headless rename was not observable",
    )?;
    service.containers().remove("renamed").await?;
    require(
        service.containers().list().await?.is_empty(),
        "headless remove left metadata",
    )?;
    println!("PASS headless-lifecycle");
    Ok(())
}
