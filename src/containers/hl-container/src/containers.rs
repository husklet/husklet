mod builder;
mod catalog;
mod configuration;
mod execution;
mod filesystem;
mod image;
mod lifecycle;

pub use builder::Builder;
#[cfg(test)]
pub(crate) use builder::test_containers;
pub use filesystem::FilesystemUsage;
pub use image::CommitMetadata;
pub use lifecycle::{LifecycleAction, LifecycleEvent, LifecycleEvents};

use crate::{Config, service::Service};
use std::sync::Arc;

/// Cheaply clonable, headless container lifecycle service.
#[derive(Clone)]
pub struct Containers {
    service: Arc<Service>,
    volumes: crate::Volumes,
    networks: crate::Networks,
}

impl Containers {
    #[must_use]
    pub fn builder(config: Config) -> Builder {
        Builder::new(config)
    }

    /// Returns the local-volume service sharing this persistence root.
    #[must_use]
    pub fn volumes(&self) -> crate::Volumes {
        self.volumes.clone()
    }

    #[must_use]
    pub fn networks(&self) -> crate::Networks {
        self.networks.clone()
    }
}

#[cfg(test)]
mod tests;
