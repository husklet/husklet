//! Container metadata durability across in-process service restarts.

use crate::api::support::require;
use hl_container::{Config, ContainerSpec, Containers, Process};
use tempfile::TempDir;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let state = work.path().join("state");
    let rootfs = work.path().join("rootfs");
    std::fs::create_dir(&rootfs)?;
    let identity = {
        let containers = Containers::builder(Config::new(&state)).build().await?;
        containers
            .create(ContainerSpec::from_directory(&rootfs, Process::new("/bin/true")).name("durable"))
            .await?
            .id
    };

    let reopened = Containers::builder(Config::new(&state)).build().await?;
    let restored = reopened.inspect("durable").await?;
    require(restored.id == identity, "restart changed container identity")?;
    require(
        reopened.list().await?.len() == 1,
        "restart did not restore exactly one record",
    )?;
    reopened.rename("durable", "after-restart").await?;
    drop(reopened);

    let reopened = Containers::builder(Config::new(&state)).build().await?;
    require(
        reopened.inspect("after-restart").await?.id == identity,
        "rename was not durable across a second restart",
    )?;
    reopened.remove("after-restart").await?;
    drop(reopened);
    require(
        Containers::builder(Config::new(&state))
            .build()
            .await?
            .list()
            .await?
            .is_empty(),
        "removal was not durable across restart",
    )?;
    println!("PASS persistence-restart");
    Ok(())
}
