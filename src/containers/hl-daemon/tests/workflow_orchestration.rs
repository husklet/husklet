//! Rust orchestration replacing shell-era end-to-end workflows.

mod workflows;

use workflows::{build, compose, network};

use hl_container::{Config, Containers};
use tempfile::TempDir;

type Error = Box<dyn std::error::Error>;

/// The workflows drive real guest programs out of the pinned Alpine minirootfs,
/// so a host without the fixture has nothing to orchestrate.
fn unavailable() -> bool {
    let missing = std::env::var_os("HL_ALPINE_ARCHIVE").is_none();
    if missing {
        println!("SKIP: HL_ALPINE_ARCHIVE is unset; the workflow orchestration has no rootfs");
    }
    missing
}

async fn containers(work: &TempDir) -> Result<Containers, Error> {
    Ok(Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?)
}

/// Currently RED, and deliberately left so: the builder's `RUN` step fails with
/// `Construction(Start)` (guest launch) while the sibling workflows start real
/// containers from the same fixture, so the defect is in the daemon build path.
#[tokio::test(flavor = "multi_thread")]
async fn docker_build() -> Result<(), Error> {
    if unavailable() {
        return Ok(());
    }
    let work = TempDir::new()?;
    build::run(&containers(&work).await?).await
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_net() -> Result<(), Error> {
    if unavailable() {
        return Ok(());
    }
    let work = TempDir::new()?;
    network::run(&containers(&work).await?).await
}

#[tokio::test(flavor = "multi_thread")]
async fn compose_project() -> Result<(), Error> {
    if unavailable() {
        return Ok(());
    }
    let work = TempDir::new()?;
    compose::run(&containers(&work).await?).await
}

#[tokio::test(flavor = "multi_thread")]
async fn compose_multinet() -> Result<(), Error> {
    if unavailable() {
        return Ok(());
    }
    let work = TempDir::new()?;
    compose::multinet(&containers(&work).await?).await
}
