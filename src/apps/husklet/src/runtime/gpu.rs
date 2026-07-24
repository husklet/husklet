//! GPU driver composition for a workspace launch.
//!
//! Driver crates own guest-library injection and API-specific configuration. This module is the one
//! product-level place that decides which drivers a workspace combines.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

#[path = "gpu_service.rs"]
mod service;

pub use service::{Backend, Configuration, Service};

/// Product composition of host graphics services and their declarative guest requirements.
pub struct Graphics {
    request: hl_container::DeviceRequest,
    library_path: String,
    _service: Service,
    _compositor: Option<crate::runtime::compositor::Service>,
}

impl Graphics {
    pub fn for_workspace(workspace: &crate::config::WorkspaceConfig) -> io::Result<Option<Self>> {
        let enabled = workspace.gui || workspace.cuda.is_some();
        if !enabled {
            return Ok(None);
        }
        let token = hl_ws::Workspace::storage_component(&workspace.name);
        let state_root = crate::paths::hl_root();
        let drivers = crate::runtime::drivers::Drivers::open(
            crate::paths::drivers_dir(),
            workspace.arch,
            workspace.gui,
            workspace.cuda.is_some(),
        )?;
        let socket =
            crate::paths::run_dir().join(format!("gpu-{token}-{}.sock", std::process::id()));
        let wayland =
            crate::paths::run_dir().join(format!("wayland-{token}-{}", std::process::id()));
        let service = Service::start(&socket, Configuration::configured()?)?;
        let compositor = workspace
            .gui
            .then(|| {
                crate::runtime::compositor::Service::start_with(
                    &wayland,
                    workspace.storage_dir(&state_root).join("frames"),
                    crate::runtime::compositor::Presentation::configured()?,
                )
            })
            .transpose()?;
        let library = match workspace.arch {
            hl_ws::Arch::Arm64 => "/usr/lib/aarch64-linux-gnu",
            hl_ws::Arch::Amd64 => "/usr/lib/x86_64-linux-gnu",
        };
        let mut namespace = vec![hl_container::device::extension::NamespaceEntry::Socket(
            hl_container::device::extension::SocketEntry {
                path: "/run/hl-gpu.sock".into(),
                host: socket,
            },
        )];
        let mut request = hl_container::DeviceRequest {
            environment: BTreeMap::from([(
                "HL_GPU_EXEC".to_owned(),
                "/run/hl-gpu.sock".to_owned(),
            )]),
            ..Default::default()
        };
        if workspace.gui {
            for (family, source, target) in [
                ("gl", "libEGL.so.1", "libEGL.so.1"),
                ("gl", "libEGL.so.1", "libEGL.so"),
                ("gl", "libGLESv2.so.2", "libGLESv2.so.2"),
                ("gl", "libGLESv2.so.2", "libGLESv2.so"),
                ("vulkan", "libvk_hl.so.1", "libvk_hl.so.1"),
                ("vulkan", "libvk_hl.so.1", "libvk_hl.so"),
                ("vulkan", "icd.json", "hl_vulkan_icd.json"),
            ] {
                namespace.push(hl_container::device::extension::NamespaceEntry::HostBind(
                    hl_container::device::extension::HostBindEntry {
                        path: format!("{library}/{target}").into(),
                        host: drivers.path(family, source),
                        access: hl_container::device::extension::BindAccess::ReadOnly,
                    },
                ));
            }
            namespace.push(hl_container::device::extension::NamespaceEntry::Socket(
                hl_container::device::extension::SocketEntry {
                    path: "/run/wayland-0".into(),
                    host: wayland,
                },
            ));
            request.environment.extend([
                ("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned()),
                ("XDG_RUNTIME_DIR".to_owned(), "/run".to_owned()),
                (
                    "VK_ICD_FILENAMES".to_owned(),
                    format!("{library}/hl_vulkan_icd.json"),
                ),
            ]);
        }
        if let Some(cuda) = &workspace.cuda {
            for (family, library_name) in [
                ("cuda", "libcuda.so.1"),
                ("cuda", "libcudart.so.1"),
                ("nvml", "libnvidia-ml.so.1"),
            ] {
                namespace.push(hl_container::device::extension::NamespaceEntry::HostBind(
                    hl_container::device::extension::HostBindEntry {
                        path: format!("{library}/{library_name}").into(),
                        host: drivers.path(family, library_name),
                        access: hl_container::device::extension::BindAccess::ReadOnly,
                    },
                ));
            }
            request.environment.extend([
                ("HL_CUDA_NAME".to_owned(), cuda.name.clone()),
                ("HL_CUDA_CC".to_owned(), cuda.compute_capability.clone()),
                (
                    "HL_CUDA_VRAM_BYTES".to_owned(),
                    u64::from(cuda.vram_mb)
                        .saturating_mul(1024 * 1024)
                        .to_string(),
                ),
            ]);
        }
        let provider = hl_container::device::extension::ProviderId::new("engine.namespace")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let host_bind = hl_container::device::extension::Feature::new("host-bind-read-only")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let sockets = hl_container::device::extension::Feature::new("unix-sockets")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        request
            .extensions
            .push(hl_container::device::extension::ExtensionSpec {
                provider,
                version: hl_container::device::Version::new(1, 0),
                required: true,
                required_features: BTreeSet::from([host_bind, sockets]),
                optional_features: BTreeSet::new(),
                config: hl_container::device::extension::ExtensionConfig::empty(
                    "engine.namespace/v1",
                ),
                namespace,
                services: Vec::new(),
                memory: Vec::new(),
                environment: Vec::new(),
            });
        Ok(Some(Self {
            request,
            library_path: library.to_owned(),
            _service: service,
            _compositor: compositor,
        }))
    }
}

impl hl_container::Device for Graphics {
    fn name(&self) -> &str {
        "graphics"
    }

    fn request(
        &self,
        context: hl_container::DeviceContext<'_>,
    ) -> hl_container::Result<hl_container::DeviceRequest> {
        let mut request = self.request.clone();
        let library_path = context
            .process
            .env
            .get("LD_LIBRARY_PATH")
            .filter(|value| !value.is_empty())
            .map_or_else(
                || self.library_path.clone(),
                |value| format!("{}:{value}", self.library_path),
            );
        request
            .environment
            .insert("LD_LIBRARY_PATH".to_owned(), library_path);
        Ok(request)
    }
}
