//! Process and image plumbing owned by the repository end-to-end runner.

use hl_container::{Config, Containers};
use std::path::Path;

pub(crate) async fn containers_for(root: &Path) -> Result<Containers, hl_container::Error> {
    Containers::builder(Config::new(root.join("state"))).build().await
}

pub(crate) async fn unpack(
    archive: std::path::PathBuf,
    destination: std::path::PathBuf,
) -> Result<(), std::io::Error> {
    tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
        std::fs::create_dir(&destination)?;
        let file = std::fs::File::open(archive)?;
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(destination)?;
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}
