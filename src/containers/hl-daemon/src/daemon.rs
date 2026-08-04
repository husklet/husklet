use std::path::Path;
use std::sync::Arc;

use hl_container::Containers as RuntimeContainers;
use hl_images::Platform;
use hl_images::remote::{Auth, Registry, Source};

use crate::Server;
use crate::events::Events;
use crate::process::{ProcessSampler, Unavailable};

/// Immutable build metadata supplied by an application composition root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    pub(crate) version: String,
}

impl Release {
    #[must_use]
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
        }
    }
}

impl Default for Release {
    fn default() -> Self {
        Self::new("development")
    }
}

/// Owns one container service graph and exposes direct or Docker-server entry points.
#[derive(Clone)]
pub struct Daemon {
    containers: RuntimeContainers,
    platform: Platform,
    source: Arc<dyn Source>,
    events: Events,
    release: Release,
    sampler: Arc<dyn ProcessSampler>,
}

impl Daemon {
    #[must_use]
    pub fn new(containers: RuntimeContainers) -> Self {
        let events = Events::new();
        containers.observe(Arc::new(events.clone()));
        Self {
            containers,
            platform: Platform::linux_arm64(),
            source: Arc::new(Registry::new(Auth::Anonymous)),
            events,
            release: Release::default(),
            sampler: Arc::new(Unavailable),
        }
    }

    /// Select the Linux guest platform used for image resolution and container creation.
    #[must_use]
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// Replace the anonymous production registry with another OCI content source.
    #[must_use]
    pub fn image_source(mut self, source: impl Source + 'static) -> Self {
        self.source = Arc::new(source);
        self
    }

    /// Supply application-owned release metadata for Docker version responses.
    #[must_use]
    pub fn release(mut self, release: Release) -> Self {
        self.release = release;
        self
    }

    /// Supply application-owned host process accounting.
    #[must_use]
    pub fn process_sampler(mut self, sampler: impl ProcessSampler) -> Self {
        self.sampler = Arc::new(sampler);
        self
    }

    /// Direct access to the exact service instance exposed by server mode.
    #[must_use]
    pub fn headless(&self) -> Containers {
        Containers {
            containers: self.containers.clone(),
        }
    }

    /// Configure a Unix-socket server over the same service instance.
    #[must_use]
    pub fn server(&self, socket: impl AsRef<Path>) -> Server {
        Server::new(
            socket.as_ref(),
            self.containers.clone(),
            self.platform.clone(),
            self.source.clone(),
            self.events.clone(),
            self.release.clone(),
            self.sampler.clone(),
        )
    }
}

/// Direct, transport-free daemon mode.
#[derive(Clone)]
pub struct Containers {
    containers: RuntimeContainers,
}

impl Containers {
    #[must_use]
    pub fn containers(&self) -> &RuntimeContainers {
        &self.containers
    }
}
