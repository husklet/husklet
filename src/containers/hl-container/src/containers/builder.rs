use super::Containers;
use crate::{
    Config, Persistence, Result,
    engine::Engine,
    service::{Dependencies, Runtime, Service},
    storage::{Disk, Memory, Storage},
};
use std::sync::Arc;

/// Composes a container service and its production integrations.
pub struct Builder {
    config: Config,
    images: Option<hl_images::Images>,
    checkpoints: Option<Arc<dyn crate::CheckpointImages>>,
}

struct Assembly<S> {
    storage: Arc<S>,
    runtime: Arc<dyn Runtime>,
    rootfs: Option<hl_images::rootfs::Roots>,
    images: Option<hl_images::Images>,
    volume_root: std::path::PathBuf,
    runtime_root: std::path::PathBuf,
    translation_cache: Option<std::path::PathBuf>,
    translation_cache_observability: bool,
    translation_symbols: Option<std::path::PathBuf>,
    checkpoints: Arc<dyn crate::CheckpointImages>,
}

impl<S: Storage + 'static> Assembly<S> {
    async fn build(self) -> Result<Containers> {
        let volume_storage: Arc<dyn crate::storage::VolumeStore> = self.storage.clone();
        let container_storage: Arc<dyn crate::storage::Containers> = self.storage.clone();
        let volumes = crate::Volumes::open(volume_storage, container_storage, self.volume_root).await?;
        let network_storage: Arc<dyn crate::storage::NetworkStore> = self.storage.clone();
        let network_containers: Arc<dyn crate::storage::Containers> = self.storage.clone();
        let networks = crate::Networks::new(
            network_storage,
            network_containers,
            volumes.operation(),
            self.runtime_root.clone(),
        );
        let bridge = crate::Subnet::new(std::net::Ipv4Addr::new(172, 17, 0, 0), 16)?;
        networks
            .ensure_predefined(crate::NetworkSpec::bridge("bridge", bridge))
            .await?;
        networks.ensure_predefined(crate::NetworkSpec::none("none")).await?;
        networks.reconcile().await?;
        let service = Arc::new(Service::new(Dependencies {
            storage: self.storage,
            runtime: self.runtime,
            rootfs: self.rootfs,
            images: self.images,
            volumes: volumes.clone(),
            networks: networks.clone(),
            runtime_root: self.runtime_root,
            translation_cache: self.translation_cache,
            translation_cache_observability: self.translation_cache_observability,
            translation_symbols: self.translation_symbols,
            checkpoints: self.checkpoints,
        }));
        service.reconcile().await?;
        service.recover().await?;
        Ok(Containers {
            service,
            volumes,
            networks,
        })
    }
}

impl Builder {
    pub(super) fn new(config: Config) -> Self {
        Self {
            config,
            images: None,
            checkpoints: None,
        }
    }

    /// Uses an existing OCI content, metadata, snapshot, and lease service.
    #[must_use]
    pub fn images(mut self, value: hl_images::Images) -> Self {
        self.images = Some(value);
        self
    }

    /// Uses application-owned durable checkpoint object storage.
    #[must_use]
    pub fn checkpoints(mut self, value: Arc<dyn crate::CheckpointImages>) -> Self {
        self.checkpoints = Some(value);
        self
    }

    /// Builds the service, opens storage, and reconciles unowned running records.
    ///
    /// # Errors
    /// Returns storage initialization, recovery, or record-validation failures.
    pub async fn build(self) -> Result<Containers> {
        let root = self.config.root;
        let translation_cache = self
            .config
            .translation_cache
            .map(crate::config::TranslationCache::prepare)
            .transpose()?;
        let translation_cache_observability = self.config.translation_cache_observability;
        let translation_symbols = self.config.translation_symbols.map(crate::config::TranslationCache::prepare).transpose()?;
        let volume_root = root.join("volumes");
        let runtime_root = root.join("runtime");
        let checkpoints = match self.checkpoints {
            Some(value) => value,
            None => Arc::new(crate::checkpoint::DirectoryImages::open(
                runtime_root.join("checkpoints"),
            )?),
        };
        let images = match self.images {
            Some(value) => value,
            None => hl_images::Images::open(root.join("images"))?,
        };
        let rootfs = images.roots();
        match self.config.persistence {
            Persistence::File => {
                Assembly {
                    storage: Arc::new(Disk::open(root).await?),
                    runtime: Arc::new(Engine::default()),
                    rootfs: Some(rootfs),
                    images: Some(images),
                    volume_root,
                    runtime_root,
                    translation_cache,
                    translation_cache_observability,
                    translation_symbols,
                    checkpoints,
                }
                .build()
                .await
            }
            Persistence::Memory => {
                Assembly {
                    storage: Arc::new(Memory::default()),
                    runtime: Arc::new(Engine::default()),
                    rootfs: Some(rootfs),
                    images: Some(images),
                    volume_root,
                    runtime_root,
                    translation_cache,
                    translation_cache_observability,
                    translation_symbols,
                    checkpoints,
                }
                .build()
                .await
            }
        }
    }
}

#[cfg(test)]
pub(super) async fn build_with<S: Storage + 'static>(
    storage: Arc<S>,
    runtime: Arc<dyn Runtime>,
    rootfs: Option<hl_images::rootfs::Roots>,
    images: Option<hl_images::Images>,
    volume_root: std::path::PathBuf,
    runtime_root: std::path::PathBuf,
) -> Result<Containers> {
    let checkpoints = Arc::new(crate::checkpoint::DirectoryImages::open(
        runtime_root.join("checkpoints"),
    )?);
    Assembly {
        storage,
        runtime,
        rootfs,
        images,
        volume_root,
        runtime_root,
        translation_cache: None,
        translation_cache_observability: false,
        translation_symbols: None,
        checkpoints,
    }
    .build()
    .await
}

#[cfg(test)]
pub(crate) async fn test_containers(
    storage: Arc<impl Storage + 'static>,
    runtime: Arc<dyn Runtime>,
) -> Result<Containers> {
    let root = std::env::temp_dir().join(format!("hl-container-{}", uuid::Uuid::new_v4()));
    let runtime_root = root.join("runtime");
    build_with(storage, runtime, None, None, root, runtime_root).await
}
