//! Launch-scoped host capabilities contributed by composed device backends.

use crate::{Guest, Mount, Process, Result};
use std::{collections::BTreeMap, sync::Arc};

pub use hl_engine::extension;
pub use hl_engine::spec::Version;

/// Read-only launch state supplied to a device backend.
#[derive(Clone, Copy)]
pub struct DeviceContext<'launch> {
    pub guest: Guest,
    pub process: &'launch Process,
}

/// Launch-scoped live ports granted to one engine extension provider.
#[derive(Clone)]
pub struct Authority {
    pub provider: extension::ProviderId,
    pub handles: Option<Arc<dyn extension::Handles>>,
    pub memory: Option<Arc<dyn extension::Memory>>,
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Authority")
            .field("provider", &self.provider)
            .field("handles", &self.handles.is_some())
            .field("memory", &self.memory.is_some())
            .finish_non_exhaustive()
    }
}

impl Authority {
    #[must_use]
    pub fn new(provider: extension::ProviderId) -> Self {
        Self {
            provider,
            handles: None,
            memory: None,
        }
    }

    #[must_use]
    pub fn handles(mut self, handles: Arc<dyn extension::Handles>) -> Self {
        self.handles = Some(handles);
        self
    }

    #[must_use]
    pub fn memory(mut self, memory: Arc<dyn extension::Memory>) -> Self {
        self.memory = Some(memory);
        self
    }
}

/// Compatibility input for providers that expose only open-handle services.
#[derive(Clone)]
pub struct HandleAuthority {
    pub provider: extension::ProviderId,
    pub handles: Arc<dyn extension::Handles>,
}

impl From<HandleAuthority> for Authority {
    fn from(authority: HandleAuthority) -> Self {
        Self::new(authority.provider).handles(authority.handles)
    }
}

/// Runtime-neutral additions requested by one device backend.
#[derive(Clone, Debug, Default)]
pub struct DeviceRequest {
    pub mounts: Vec<Mount>,
    pub environment: BTreeMap<String, String>,
    pub extensions: Vec<extension::ExtensionSpec>,
    pub authorities: Vec<Authority>,
}

impl DeviceRequest {
    fn validate(&self) -> Result<()> {
        let mut mounts = std::collections::BTreeSet::new();
        for mount in &self.mounts {
            if !mounts.insert(&mount.target) {
                return Err(crate::Error::InvalidSpec(format!(
                    "device mount target is duplicated: {}",
                    mount.target.display()
                )));
            }
        }
        let mut extensions = std::collections::BTreeSet::new();
        for spec in &self.extensions {
            if !extensions.insert(spec.provider.clone()) {
                return Err(crate::Error::InvalidSpec(format!(
                    "device extension provider is duplicated: {}",
                    spec.provider.as_str()
                )));
            }
        }
        let mut authorities = std::collections::BTreeSet::new();
        for authority in &self.authorities {
            if !authorities.insert(authority.provider.clone()) {
                return Err(crate::Error::InvalidSpec(format!(
                    "device provider authority is duplicated: {}",
                    authority.provider.as_str()
                )));
            }
        }
        Ok(())
    }
}

/// A backend that describes the host capabilities needed by a container launch.
pub trait Device: Send + Sync {
    fn name(&self) -> &str;

    /// Resolves this backend for one launch without teaching the container runtime its domain.
    ///
    /// # Errors
    /// Returns a typed container error when the backend cannot support the selected launch.
    fn request(&self, context: DeviceContext<'_>) -> Result<DeviceRequest>;
}

/// Ordered device backends selected by the application composition root.
#[derive(Clone, Default)]
pub struct Devices(Vec<Arc<dyn Device>>);

impl Devices {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, device: impl Device + 'static) -> Self {
        self.0.push(Arc::new(device));
        self
    }

    pub fn add(&mut self, device: impl Device + 'static) {
        self.0.push(Arc::new(device));
    }

    pub(crate) fn request(&self, context: DeviceContext<'_>) -> Result<DeviceRequest> {
        let mut combined = DeviceRequest::default();
        for device in &self.0 {
            let request = device.request(context).map_err(|error| {
                crate::Error::InvalidSpec(format!("device {:?}: {error}", device.name()))
            })?;
            combined.mounts.extend(request.mounts);
            combined.environment.extend(request.environment);
            combined.extensions.extend(request.extensions);
            combined.authorities.extend(request.authorities);
        }
        combined.validate()?;
        Ok(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Console;
    use std::path::PathBuf;

    #[derive(Clone)]
    struct Graphics(DeviceRequest);

    impl Device for Graphics {
        fn name(&self) -> &'static str {
            "graphics"
        }

        fn request(&self, _context: DeviceContext<'_>) -> Result<DeviceRequest> {
            Ok(self.0.clone())
        }
    }

    fn process() -> Process {
        Process {
            program: "/bin/true".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_dir: PathBuf::from("/"),
            uid: None,
            gid: None,
            console: Console::default(),
        }
    }

    #[test]
    fn devices_compose_launch_requirements() {
        let request = DeviceRequest {
            mounts: vec![Mount::read_write("/host/gpu.sock", "/run/gpu.sock")],
            environment: BTreeMap::from([("GPU_SOCKET".to_owned(), "/run/gpu.sock".to_owned())]),
            ..Default::default()
        };
        let devices = Devices::new().with(Graphics(request));
        let process = process();

        let combined = devices
            .request(DeviceContext {
                guest: Guest::Aarch64,
                process: &process,
            })
            .unwrap();

        assert_eq!(combined.mounts[0].target, PathBuf::from("/run/gpu.sock"));
        assert_eq!(combined.environment["GPU_SOCKET"], "/run/gpu.sock");
    }

    #[test]
    fn devices_reject_duplicate_mount_targets() {
        let first = DeviceRequest {
            mounts: vec![Mount::read_only("/host/first", "/guest/library")],
            ..Default::default()
        };
        let second = DeviceRequest {
            mounts: vec![Mount::read_only("/host/second", "/guest/library")],
            ..Default::default()
        };
        let devices = Devices::new().with(Graphics(first)).with(Graphics(second));
        let process = process();

        let error = devices
            .request(DeviceContext {
                guest: Guest::Aarch64,
                process: &process,
            })
            .unwrap_err();

        assert!(error.to_string().contains("mount target is duplicated"));
    }
}
