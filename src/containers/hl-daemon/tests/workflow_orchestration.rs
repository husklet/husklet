//! Rust orchestration replacing shell-era end-to-end workflows.

#[path = "workflow/mod.rs"]
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

/// Still RED, but not for the reason this comment used to give. That reading was taken
/// on an arm64 Mac, where `Daemon::new`'s hardcoded `Platform::linux_arm64()` happened to
/// match the arm64 minirootfs the Darwin dev shell pins; on x86_64 Linux the same default
/// refused the amd64 fixture at the door with `no manifest for platform linux/arm64`, and
/// no build step ran at all. With the platform taken from the fixture, both `RUN` builds
/// complete and their containers produce their expected bytes.
///
/// What remains is one step further in: `build::advanced` builds `workflow/advanced:test`
/// (`USER nobody`, `SHELL ["/bin/sh","-eu","-c"]`, `ENTRYPOINT ["/bin/sh","-c"]`), the
/// image, its labels and its history all verify, the container is created and started --
/// and `wait` never returns. `Timeout` here is the client's own 30s request budget, not a
/// daemon answer. That is a non-root guest lifecycle question for `hl-container`, not a
/// fixture one.
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
