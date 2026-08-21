//! Rust orchestration replacing shell-era end-to-end workflows.

#[path = "workflow/mod.rs"]
mod workflows;

use workflows::{build, compose, network, sandbox};

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

/// The `USER nobody` / `SHELL` / `ENTRYPOINT` image `build::advanced` produces was the first workflow
/// here whose guest ran a command substitution, and it hung at `wait` for as long as this fixture has
/// existed. The cause was not the non-root user: under the production sandbox default the sentry filed
/// a forked child's descriptor table under the clone's GUEST pid while every request that child made
/// was stamped with its HOST pid, so no child ever inherited a descriptor. See `sandbox_process_tree`,
/// which pins the mechanism in about a second instead of through an image build.
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

/// A guest process tree under the production sandbox default. `Sandbox::SentryOnly` is what an
/// ordinary container gets, and every other workflow in this file either disables it or reaches it
/// only through a long end-to-end path -- so a descriptor defect behind it presented as an image
/// build hanging and as a network fixture whose listeners never served, never as itself.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_process_tree() -> Result<(), Error> {
    if unavailable() {
        return Ok(());
    }
    let work = TempDir::new()?;
    sandbox::run(&containers(&work).await?).await
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
