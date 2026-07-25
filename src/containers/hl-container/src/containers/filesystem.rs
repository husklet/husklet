use super::Containers;
use crate::Result;
use hl_fs::Directory;
use std::io::Seek as _;

/// Logical rootfs and writable-layer byte usage for one container.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemUsage {
    pub writable: u64,
    pub rootfs: u64,
}

impl Containers {
    /// Opens the container's mount-aware filesystem surface.
    ///
    /// # Errors
    /// Returns lookup, rootfs ownership, or filesystem validation failures.
    pub async fn filesystem(&self, reference: &str) -> Result<crate::Filesystem> {
        self.service.filesystem(reference).await
    }

    /// Accounts the container's merged rootfs and private writable layer.
    ///
    /// # Errors
    /// Returns lookup, rootfs ownership, archive, or filesystem failures.
    pub async fn filesystem_usage(&self, reference: &str) -> Result<FilesystemUsage> {
        let container = self.inspect(reference).await?;
        let writable = match &container.spec.rootfs {
            crate::Rootfs::Image(rootfs) if rootfs.overlay().is_some() => Some(
                self.images()?
                    .roots()
                    .open_overlay(rootfs)?
                    .upper()
                    .to_owned(),
            ),
            crate::Rootfs::Image(_) | crate::Rootfs::Directory(_) => None,
        };
        let filesystem = self.filesystem(reference).await?;
        tokio::task::spawn_blocking(move || {
            let writable = writable
                .as_deref()
                .map(|path| Directory::from(path).size())
                .transpose()?
                .unwrap_or_default();
            let mut archive = tempfile::tempfile()?;
            filesystem.archive("/", &mut archive)?;
            archive.rewind()?;
            let mut rootfs = 0_u64;
            for entry in tar::Archive::new(archive).entries()? {
                let entry = entry?;
                if entry.header().entry_type().is_file() {
                    rootfs = rootfs.saturating_add(entry.size());
                }
            }
            Ok(FilesystemUsage { writable, rootfs })
        })
        .await
        .map_err(|error| crate::Error::Io(std::io::Error::other(error)))?
    }

    /// Lists paths added, modified, or deleted relative to an image-backed rootfs baseline.
    ///
    /// # Errors
    /// Returns lookup, baseline ownership, or filesystem failures. Directory-backed containers do
    /// not have an implicit baseline and are rejected.
    pub async fn changes(&self, reference: &str) -> Result<crate::Changes> {
        self.service.changes(reference).await
    }
}
