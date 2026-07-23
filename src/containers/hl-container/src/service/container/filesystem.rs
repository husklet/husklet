use super::{Error, Result, Rootfs, Service};

impl Service {
    pub(crate) async fn filesystem(&self, reference: &str) -> Result<crate::Filesystem> {
        let container = self.resolve(reference).await?;
        let mounts = self.volumes.resolve(&container.spec.mounts).await?;
        let generation = self.identity.generation(&container)?;
        let filesystem = match &container.spec.rootfs {
            Rootfs::Image(reference) if reference.overlay().is_some() => {
                let manager = self.rootfs.clone().ok_or_else(|| {
                    Error::InvalidSpec("image rootfs requires a configured rootfs manager".into())
                })?;
                let reference = reference.clone();
                let (lower, upper, lower_ownership, upper_ownership) =
                    tokio::task::spawn_blocking(move || {
                        manager.open_overlay(&reference).map(|view| {
                            (
                                view.lower().to_owned(),
                                view.upper().to_owned(),
                                view.lower_ownership().clone(),
                                view.upper_ownership().clone(),
                            )
                        })
                    })
                    .await
                    .map_err(|error| Error::Io(std::io::Error::other(error)))??;
                crate::Filesystem::overlay(lower, upper, lower_ownership, upper_ownership, mounts)
            }
            _ => crate::Filesystem::new(self.rootfs_path(&container.spec.rootfs).await?, mounts),
        };
        Ok(filesystem.with_generation(generation))
    }

    pub(crate) async fn changes(&self, reference: &str) -> Result<crate::Changes> {
        let container = self.resolve(reference).await?;
        if container.state.is_active() {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "stopped for a coherent filesystem comparison",
            });
        }
        let Rootfs::Image(reference) = container.spec.rootfs else {
            return Err(Error::InvalidSpec(
                "filesystem changes require an image-backed rootfs".into(),
            ));
        };
        let manager = self.rootfs.clone().ok_or_else(|| {
            Error::InvalidSpec("image rootfs requires a configured rootfs manager".into())
        })?;
        tokio::task::spawn_blocking(move || {
            manager
                .changes(&reference)?
                .into_iter()
                .map(|change| {
                    let kind = match change.kind {
                        hl_images::rootfs::ChangeKind::Modified => crate::ChangeKind::Modified,
                        hl_images::rootfs::ChangeKind::Added => crate::ChangeKind::Added,
                        hl_images::rootfs::ChangeKind::Deleted => crate::ChangeKind::Deleted,
                    };
                    Ok(crate::Change {
                        path: change.path,
                        kind,
                    })
                })
                .collect::<Result<Vec<_>>>()
                .map(crate::Changes::from)
        })
        .await
        .map_err(|error| Error::Io(std::io::Error::other(error)))?
    }

    pub(super) async fn rootfs_path(&self, rootfs: &Rootfs) -> Result<std::path::PathBuf> {
        self.rootfs_launch(rootfs).await.map(|(path, _, _)| path)
    }

    pub(super) async fn rootfs_launch(
        &self,
        rootfs: &Rootfs,
    ) -> Result<(
        std::path::PathBuf,
        Option<crate::service::OverlayConfig>,
        Vec<(std::path::PathBuf, u32, u32)>,
    )> {
        match rootfs {
            Rootfs::Directory(path) => Ok((path.clone(), None, Vec::new())),
            Rootfs::Image(reference) => {
                let manager = self.rootfs.clone().ok_or_else(|| {
                    Error::InvalidSpec("image rootfs requires a configured rootfs manager".into())
                })?;
                let reference = reference.clone();
                tokio::task::spawn_blocking(move || {
                    if reference.overlay().is_some() {
                        return manager.open_overlay(&reference).map(|handle| {
                            let owners = handle
                                .lower_ownership()
                                .iter()
                                .chain(handle.upper_ownership().iter())
                                .map(|(path, owner)| (path.to_owned(), owner.uid, owner.gid))
                                .collect();
                            let overlay = crate::service::OverlayConfig {
                                lower: handle.lower().to_owned(),
                                upper: handle.upper().to_owned(),
                                work: handle.work().to_owned(),
                            };
                            (overlay.lower.clone(), Some(overlay), owners)
                        });
                    }
                    manager.open(&reference).map(|handle| {
                        let owners = handle
                            .ownership()
                            .iter()
                            .map(|(path, owner)| (path.to_owned(), owner.uid, owner.gid))
                            .collect();
                        (handle.path().to_owned(), None, owners)
                    })
                })
                .await
                .map_err(|error| Error::Io(std::io::Error::other(error)))?
                .map_err(Error::Image)
            }
        }
    }
}
