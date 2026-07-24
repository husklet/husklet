use crate::model::now_ms;
use crate::storage::VolumeStore;
use crate::{
    model::ResolvedMount, Access, Error, Mount, MountSource, Result, Volume, VolumeKind,
    VolumeSource, VolumeSpec,
};
use hl_fs::Directory;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[path = "volume_transaction.rs"]
mod transaction;

/// Locally managed durable volumes.
#[derive(Clone)]
pub struct Volumes {
    storage: Arc<dyn VolumeStore>,
    containers: Arc<dyn crate::storage::Containers>,
    root: Arc<PathBuf>,
    operation: Arc<Mutex<()>>,
}

impl Volumes {
    pub(crate) async fn open(
        storage: Arc<dyn VolumeStore>,
        containers: Arc<dyn crate::storage::Containers>,
        root: PathBuf,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(&root).await?;
        let root = tokio::fs::canonicalize(root).await?;
        let volumes = Self {
            storage,
            containers,
            root: Arc::new(root),
            operation: Arc::new(Mutex::new(())),
        };
        volumes.reconcile().await?;
        Ok(volumes)
    }

    /// Create a local volume and its private data directory.
    ///
    /// # Errors
    /// Returns validation, name-conflict, filesystem, or persistence failures.
    pub async fn create(&self, spec: VolumeSpec) -> Result<Volume> {
        spec.validate()?;
        let spec = Self::canonicalize_source(spec)?;
        let _guard = self.operation.lock().await;
        if let Some(volume) = self.storage.get(&spec.name).await? {
            if volume.labels == spec.labels
                && volume.options == spec.options
                && volume.source == spec.source
            {
                return Ok(volume);
            }
            return Err(Error::VolumeConflict(spec.name));
        }
        if let VolumeSource::Bind { device, .. } = &spec.source {
            let volume = Volume::from_spec(spec.clone(), device.clone(), now_ms());
            self.storage.insert(&volume).await?;
            return Ok(volume);
        }
        let directory = self.directory(&spec.name);
        if tokio::fs::try_exists(&directory).await? {
            return Err(Error::VolumeConflict(spec.name));
        }
        let staging = self.staging(&spec.name);
        Directory::from(&staging).remove().await?;
        tokio::fs::create_dir_all(staging.join("_data")).await?;
        Directory::from(staging.join("_data")).sync()?;
        Directory::from(&staging).sync()?;
        Directory::from(self.root.join(".create")).sync()?;
        let volume = Volume::from_spec(spec, directory.join("_data"), now_ms());
        if let Err(error) = self.storage.insert(&volume).await {
            let _ = Directory::from(&staging).remove().await;
            return Err(error);
        }
        if let Err(error) = tokio::fs::rename(&staging, &directory).await {
            let rollback = self.storage.remove(&volume.name).await;
            let _ = Directory::from(&staging).remove().await;
            return match rollback {
                Ok(()) => Err(error.into()),
                Err(rollback) => Err(Error::Corrupt(format!(
                    "volume directory publication failed ({error}); metadata rollback also failed ({rollback})"
                ))),
            };
        }
        Directory::from(self.root.as_ref()).sync()?;
        Directory::from(self.root.join(".create")).sync()?;
        Ok(volume)
    }

    /// Create a local volume with a collision-resistant generated name.
    ///
    /// # Errors
    /// Returns label-validation, filesystem, entropy, or persistence failures.
    pub async fn create_anonymous<K, V>(
        &self,
        labels: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Volume>
    where
        K: Into<String>,
        V: Into<String>,
    {
        let labels = labels
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect::<std::collections::BTreeMap<_, _>>();
        loop {
            let mut spec = VolumeSpec::new(uuid::Uuid::new_v4().simple().to_string());
            spec.kind = VolumeKind::Anonymous;
            spec.labels.clone_from(&labels);
            match self.create(spec).await {
                Err(Error::VolumeConflict(_)) => {}
                result => return result,
            }
        }
    }

    /// List volumes in deterministic name order.
    ///
    /// # Errors
    /// Returns persistence or corrupt-record failures.
    pub async fn list(&self) -> Result<Vec<Volume>> {
        let mut volumes = self.storage.list().await?;
        volumes.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(volumes)
    }

    /// Inspect a volume by its exact name.
    ///
    /// # Errors
    /// Returns validation, not-found, persistence, or corrupt-record failures.
    pub async fn inspect(&self, name: &str) -> Result<Volume> {
        VolumeSpec::new(name).validate()?;
        self.storage
            .get(name)
            .await?
            .ok_or_else(|| Error::VolumeNotFound(name.into()))
    }

    /// Count persisted containers that reference this volume.
    ///
    /// # Errors
    /// Returns validation, lookup, or persistence failures.
    pub async fn references(&self, name: &str) -> Result<usize> {
        let volume = self.inspect(name).await?;
        Ok(self
            .containers
            .list()
            .await?
            .into_iter()
            .filter(|container| container.uses_volume(&volume.name))
            .count())
    }

    /// Measure regular-file bytes currently owned by this volume.
    ///
    /// Symbolic links are measured as links and never followed.
    ///
    /// # Errors
    /// Returns validation, lookup, task, or filesystem failures.
    pub async fn size(&self, name: &str) -> Result<u64> {
        let volume = self.inspect(name).await?;
        if matches!(volume.source, VolumeSource::Bind { .. }) {
            return Ok(0);
        }
        let path = volume.path;
        Ok(
            tokio::task::spawn_blocking(move || Directory::from(path).size())
                .await
                .map_err(|error| Error::Io(std::io::Error::other(error)))??,
        )
    }

    /// Remove a volume and every entry in its managed data directory.
    ///
    /// # Errors
    /// Returns validation, not-found, persistence, or filesystem cleanup failures.
    pub async fn remove(&self, name: &str) -> Result<Volume> {
        VolumeSpec::new(name).validate()?;
        let _guard = self.operation.lock().await;
        self.remove_locked(name).await
    }

    pub(crate) async fn remove_locked(&self, name: &str) -> Result<Volume> {
        let volume = self
            .storage
            .get(name)
            .await?
            .ok_or_else(|| Error::VolumeNotFound(name.into()))?;
        if self.in_use(&volume).await? {
            return Err(Error::VolumeInUse(name.into()));
        }
        if matches!(volume.source, VolumeSource::Bind { .. }) {
            self.storage.remove(name).await?;
            return Ok(volume);
        }
        let directory = self.directory(name);
        let trash = self.trash(name);
        Directory::from(&trash).remove().await?;
        if tokio::fs::try_exists(&directory).await? {
            tokio::fs::rename(&directory, &trash).await?;
            Directory::from(self.root.as_ref()).sync()?;
            Directory::from(self.root.join(".trash")).sync()?;
        }
        if let Err(error) = self.storage.remove(name).await {
            if tokio::fs::try_exists(&trash).await.unwrap_or(false) {
                let _ = tokio::fs::rename(&trash, &directory).await;
            }
            return Err(error);
        }
        Directory::from(&trash).remove().await?;
        Directory::from(self.root.join(".trash")).sync()?;
        Ok(volume)
    }

    /// Remove every currently known volume through the normal ownership path.
    ///
    /// # Errors
    /// Returns persistence or filesystem cleanup failures.
    pub async fn prune(&self) -> Result<Vec<Volume>> {
        let volumes = self.list().await?;
        let mut removed = Vec::with_capacity(volumes.len());
        for volume in volumes {
            match self.remove(&volume.name).await {
                Ok(volume) => removed.push(volume),
                Err(Error::VolumeNotFound(_) | Error::VolumeInUse(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(removed)
    }

    fn directory(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn staging(&self, name: &str) -> PathBuf {
        self.root.join(".create").join(name)
    }

    fn trash(&self, name: &str) -> PathBuf {
        self.root.join(".trash").join(name)
    }

    async fn in_use(&self, volume: &Volume) -> Result<bool> {
        for container in self.containers.list().await? {
            if container.uses_volume(&volume.name) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) async fn resolve(&self, mounts: &[Mount]) -> Result<Vec<ResolvedMount>> {
        let mut resolved = Vec::with_capacity(mounts.len());
        for mount in mounts {
            let (source, forced_read_only) = match &mount.source {
                MountSource::Bind(path) => (path.clone(), false),
                MountSource::Volume(name)
                | MountSource::Anonymous(name)
                | MountSource::Tmpfs(name) => {
                    let volume = self.inspect(name).await?;
                    let read_only = matches!(
                        volume.source,
                        VolumeSource::Bind {
                            read_only: true,
                            ..
                        }
                    );
                    (volume.path, read_only)
                }
            };
            let source = if let Some(subpath) = &mount.subpath {
                if matches!(mount.source, MountSource::Bind(_) | MountSource::Tmpfs(_)) {
                    return Err(Error::InvalidSpec(
                        "subpath is only valid for managed volume mounts".into(),
                    ));
                }
                let root = tokio::fs::canonicalize(&source).await?;
                let selected = tokio::fs::canonicalize(source.join(subpath))
                    .await
                    .map_err(|error| {
                        Error::InvalidSpec(format!(
                            "volume subpath {} is unavailable: {error}",
                            subpath.display()
                        ))
                    })?;
                let directory = tokio::fs::metadata(&selected).await?.is_dir();
                if !selected.starts_with(&root) || !directory {
                    return Err(Error::InvalidSpec(format!(
                        "volume subpath {} must resolve to a directory inside the volume",
                        subpath.display()
                    )));
                }
                selected
            } else {
                source
            };
            resolved.push(ResolvedMount {
                source,
                target: mount.target.clone(),
                access: if forced_read_only {
                    Access::ReadOnly
                } else {
                    mount.access
                },
            });
        }
        Ok(resolved)
    }

    pub(crate) async fn validate(&self, mounts: &[Mount]) -> Result<()> {
        for mount in mounts {
            if let MountSource::Volume(name)
            | MountSource::Anonymous(name)
            | MountSource::Tmpfs(name) = &mount.source
            {
                self.storage
                    .get(name)
                    .await?
                    .ok_or_else(|| Error::VolumeNotFound(name.clone()))?;
            }
        }
        Ok(())
    }

    pub(crate) fn operation(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.operation)
    }

    fn canonicalize_source(mut spec: VolumeSpec) -> Result<VolumeSpec> {
        if let VolumeSource::Bind { device, read_only } = &spec.source {
            if !device.is_absolute() {
                return Err(Error::InvalidVolume(
                    "local bind volume device must be an absolute path".into(),
                ));
            }
            let canonical = std::fs::canonicalize(device).map_err(|error| {
                Error::InvalidVolume(format!(
                    "local bind volume device {} is unavailable: {error}",
                    device.display()
                ))
            })?;
            if !canonical.is_dir() {
                return Err(Error::InvalidVolume(format!(
                    "local bind volume device {} must be a directory",
                    device.display()
                )));
            }
            spec.source = VolumeSource::Bind {
                device: canonical,
                read_only: *read_only,
            };
        }
        Ok(spec)
    }
}
