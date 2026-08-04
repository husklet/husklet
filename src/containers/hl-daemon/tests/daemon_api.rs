//! Public daemon API integration contracts.

mod api;

use api::support::{containers_for, unpack};
use std::{env, path::PathBuf};
use tempfile::TempDir;

type Error = Box<dyn std::error::Error>;

macro_rules! api_test {
    ($name:ident, $module:ident) => {
        #[tokio::test]
        async fn $name() -> Result<(), Error> {
            api::$module::run().await
        }
    };
}

api_test!(concurrent_clients, concurrent_clients);
api_test!(container_copy, container_copy);
api_test!(headless_lifecycle, headless_lifecycle);
api_test!(http_errors, http_errors);
api_test!(image_archive, image_archive);
api_test!(image_prune, image_prune);
api_test!(malformed_image_archive, malformed_image_archive);
api_test!(persistence_restart, persistence_restart);
api_test!(removal_wait_race, removal_wait_race);

/// These contracts execute real Linux programs and therefore require the pinned
/// Alpine rootfs used by repository end-to-end runs.
#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn alpine_runtime_contracts() -> Result<(), Error> {
    let work = TempDir::new()?;
    let rootfs = work.path().join("rootfs");
    let archive = env::var_os("HL_ALPINE_ARCHIVE")
        .map(PathBuf::from)
        .ok_or("HL_ALPINE_ARCHIVE must name the pinned Alpine minirootfs")?;
    unpack(archive, rootfs.clone()).await?;
    let containers = containers_for(work.path()).await?;

    api::named_volume::run(work.path(), &rootfs).await?;
    api::headless_runtime::run(&containers, &rootfs).await?;
    api::resources::run(&containers, &rootfs).await?;
    api::network_bridge::run(&containers, &rootfs).await?;
    api::port_publishing::run(&containers, &rootfs).await?;
    api::daemon_runtime::run(containers, &rootfs, work.path()).await
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn descendant_cleanup() -> Result<(), Error> {
    api::descendant_cleanup::run().await
}
