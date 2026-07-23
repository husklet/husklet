use super::Volumes;
use crate::{Error, Mount, MountSource, Result, VolumeSource};
use hl_fs::Directory;
use std::path::Path;

impl Volumes {
    pub(crate) async fn populate(&self, mounts: &[Mount], rootfs: &Path) -> Result<()> {
        let _guard = self.operation.lock().await;
        for mount in mounts.iter().filter(|mount| mount.populate) {
            let name = match &mount.source {
                MountSource::Volume(name)
                | MountSource::Anonymous(name)
                | MountSource::Tmpfs(name) => name,
                MountSource::Bind(_) => {
                    return Err(Error::InvalidSpec(
                        "bind mounts cannot request rootfs population".into(),
                    ));
                }
            };
            let volume = self
                .storage
                .get(name)
                .await?
                .ok_or_else(|| Error::VolumeNotFound(name.clone()))?;
            if matches!(volume.source, VolumeSource::Bind { .. }) {
                continue;
            }
            if std::fs::read_dir(&volume.path)?.next().is_some() {
                continue;
            }
            let source = rootfs.join(
                mount
                    .target
                    .strip_prefix("/")
                    .map_err(|_| Error::InvalidSpec("mount target must be absolute".into()))?,
            );
            if !source.exists() {
                continue;
            }
            let canonical_root = std::fs::canonicalize(rootfs)?;
            let canonical_source = std::fs::canonicalize(&source)?;
            if !canonical_source.starts_with(&canonical_root) {
                return Err(Error::InvalidSpec(format!(
                    "volume population source {} escapes the rootfs",
                    source.display()
                )));
            }
            if let Err(error) =
                Directory::from(&canonical_source).copy_to(&Directory::from(&volume.path))
            {
                Directory::from(&volume.path).clear().await?;
                return Err(error.into());
            }
        }
        Ok(())
    }

    pub(super) async fn reconcile(&self) -> Result<()> {
        tokio::fs::create_dir_all(self.root.join(".create")).await?;
        tokio::fs::create_dir_all(self.root.join(".trash")).await?;
        let volumes = self.storage.list().await?;
        for volume in &volumes {
            if let VolumeSource::Bind { device, .. } = &volume.source {
                let canonical = std::fs::canonicalize(device).map_err(|error| {
                    Error::Corrupt(format!(
                        "volume {:?} bind device is unavailable: {error}",
                        volume.name
                    ))
                })?;
                if canonical != *device || !canonical.is_dir() {
                    return Err(Error::Corrupt(format!(
                        "volume {:?} bind device is not a stable directory",
                        volume.name
                    )));
                }
                continue;
            }
            let directory = self.directory(&volume.name);
            let staging = self.staging(&volume.name);
            let trash = self.trash(&volume.name);
            if !tokio::fs::try_exists(&directory).await? && tokio::fs::try_exists(&staging).await? {
                tokio::fs::rename(staging, &directory).await?;
            }
            if !tokio::fs::try_exists(&directory).await? && tokio::fs::try_exists(&trash).await? {
                tokio::fs::rename(trash, directory).await?;
            }
            if !tokio::fs::try_exists(&volume.path).await? {
                return Err(Error::Corrupt(format!(
                    "volume {:?} data directory is missing",
                    volume.name
                )));
            }
        }
        Directory::from(self.root.join(".create")).clear().await?;
        Directory::from(self.root.join(".trash")).clear().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Volumes;
    use crate::model::now_ms;
    use crate::storage::{Memory, VolumeStore as _};
    use crate::{Access, Mount, Volume, VolumeSpec};
    use std::sync::Arc;

    #[tokio::test]
    async fn reconciliation_completes_metadata_first_creation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("volumes");
        std::fs::create_dir_all(root.join(".create/recovered/_data")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let storage = Arc::new(Memory::default());
        let volume = Volume::from_spec(
            VolumeSpec::new("recovered"),
            root.join("recovered/_data"),
            now_ms(),
        );
        storage.insert(&volume).await.unwrap();

        let volumes = Volumes::open(storage.clone(), storage, root.clone())
            .await
            .unwrap();
        assert_eq!(volumes.inspect("recovered").await.unwrap(), volume);
        assert!(root.join("recovered/_data").is_dir());
        assert!(!root.join(".create/recovered").exists());
    }

    #[tokio::test]
    async fn reconciliation_restores_metadata_owned_trash_and_cleans_only_transactions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("volumes");
        let storage = Arc::new(Memory::default());
        let volumes = Volumes::open(storage.clone(), storage.clone(), root.clone())
            .await
            .unwrap();
        let volume = volumes.create(VolumeSpec::new("restored")).await.unwrap();
        std::fs::write(volume.path().join("payload"), b"kept").unwrap();
        std::fs::rename(root.join("restored"), root.join(".trash/restored")).unwrap();
        std::fs::create_dir_all(root.join(".trash/abandoned")).unwrap();
        std::fs::create_dir_all(root.join(".create/abandoned")).unwrap();
        std::fs::create_dir_all(root.join("unknown")).unwrap();

        let reopened = Volumes::open(storage.clone(), storage, root.clone())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(
                reopened
                    .inspect("restored")
                    .await
                    .unwrap()
                    .path()
                    .join("payload")
            )
            .unwrap(),
            b"kept"
        );
        assert!(!root.join(".trash/abandoned").exists());
        assert!(!root.join(".create/abandoned").exists());
        assert!(root.join("unknown").exists());
    }

    #[tokio::test]
    async fn population_copies_only_an_interior_rootfs_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        std::fs::create_dir_all(rootfs.join("data/nested")).unwrap();
        std::fs::write(rootfs.join("data/nested/value"), b"inside").unwrap();
        std::fs::write(temporary.path().join("outside"), b"outside").unwrap();
        let storage = Arc::new(Memory::default());
        let volumes = Volumes::open(storage.clone(), storage, temporary.path().join("volumes"))
            .await
            .unwrap();
        let volume = volumes
            .create_anonymous(std::iter::empty::<(&str, &str)>())
            .await
            .unwrap();
        volumes
            .populate(
                &[Mount::anonymous(&volume, "/data", Access::ReadWrite).populate()],
                &rootfs,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(volume.path().join("nested/value")).unwrap(),
            b"inside"
        );
        assert!(!volume.path().join("outside").exists());
    }
}
