use super::Containers;
use crate::{
    engine::Engine,
    service::{Dependencies, Runtime, Service},
    storage::{Disk, Memory, Storage},
    Config, Persistence, Result,
};
use std::sync::Arc;

/// Composes a container service and its production integrations.
pub struct Builder {
    config: Config,
    images: Option<hl_images::Images>,
    devices: crate::Devices,
}

impl Builder {
    pub(super) fn new(config: Config) -> Self {
        Self {
            config,
            images: None,
            devices: crate::Devices::new(),
        }
    }

    /// Supplies application-selected device backends to every process launched by this service.
    #[must_use]
    pub fn devices(mut self, value: crate::Devices) -> Self {
        self.devices = value;
        self
    }

    /// Uses an existing OCI content, metadata, snapshot, and lease service.
    #[must_use]
    pub fn images(mut self, value: hl_images::Images) -> Self {
        self.images = Some(value);
        self
    }

    /// Builds the service, opens storage, and reconciles unowned running records.
    ///
    /// # Errors
    /// Returns storage initialization, recovery, or record-validation failures.
    pub async fn build(self) -> Result<Containers> {
        let root = self.config.root;
        let devices = self.devices;
        let volume_root = root.join("volumes");
        let runtime_root = root.join("runtime");
        let images = match self.images {
            Some(value) => value,
            None => hl_images::Images::open(root.join("images"))?,
        };
        let rootfs = images.roots();
        match self.config.persistence {
            Persistence::File => {
                build_with(
                    Arc::new(Disk::open(root).await?),
                    Arc::new(Engine),
                    Some(rootfs),
                    Some(images),
                    volume_root,
                    runtime_root,
                    devices,
                )
                .await
            }
            Persistence::Memory => {
                build_with(
                    Arc::new(Memory::default()),
                    Arc::new(Engine),
                    Some(rootfs),
                    Some(images),
                    volume_root,
                    runtime_root,
                    devices,
                )
                .await
            }
        }
    }
}

pub(super) async fn build_with<S: Storage + 'static>(
    storage: Arc<S>,
    runtime: Arc<dyn Runtime>,
    rootfs: Option<hl_images::rootfs::Roots>,
    images: Option<hl_images::Images>,
    volume_root: std::path::PathBuf,
    runtime_root: std::path::PathBuf,
    devices: crate::Devices,
) -> Result<Containers> {
    let volume_storage: Arc<dyn crate::storage::VolumeStore> = storage.clone();
    let container_storage: Arc<dyn crate::storage::Containers> = storage.clone();
    let volumes = crate::Volumes::open(volume_storage, container_storage, volume_root).await?;
    let network_storage: Arc<dyn crate::storage::NetworkStore> = storage.clone();
    let network_containers: Arc<dyn crate::storage::Containers> = storage.clone();
    let networks = crate::Networks::new(
        network_storage,
        network_containers,
        volumes.operation(),
        runtime_root.clone(),
    );
    networks.reconcile().await?;
    let service = Arc::new(Service::new(Dependencies {
        storage,
        runtime,
        rootfs,
        images,
        volumes: volumes.clone(),
        networks: networks.clone(),
        runtime_root,
        devices,
    }));
    service.reconcile().await?;
    service.recover().await?;
    Ok(Containers {
        service,
        volumes,
        networks,
    })
}

#[cfg(test)]
pub(crate) async fn test_containers(
    storage: Arc<impl Storage + 'static>,
    runtime: Arc<dyn Runtime>,
) -> Result<Containers> {
    let root = std::env::temp_dir().join(format!("hl-container-{}", uuid::Uuid::new_v4()));
    let runtime_root = root.join("runtime");
    build_with(
        storage,
        runtime,
        None,
        None,
        root,
        runtime_root,
        crate::Devices::new(),
    )
    .await
}
