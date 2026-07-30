//! GPU driver composition for a workspace launch.
//!
//! Driver crates own guest-library injection and API-specific configuration. This module is the one
//! product-level place that decides which drivers a workspace combines.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

mod capture;
pub(crate) mod executor;
mod projection;
mod render;
pub mod replay;
#[path = "gpu_service.rs"]
mod service;

use projection::Projection;
use render::RenderNode;
pub use service::{Backend, Configuration, Service};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureOptions {
    directory: PathBuf,
    batches: u64,
    bytes: u64,
    presentations: u64,
}

impl CaptureOptions {
    pub fn new(
        directory: PathBuf,
        batches: u64,
        bytes: u64,
        presentations: u64,
    ) -> io::Result<Self> {
        if directory.as_os_str().is_empty() || batches == 0 || bytes < 20 || presentations == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GPU capture path and limits must be positive",
            ));
        }
        Ok(Self {
            directory,
            batches,
            bytes,
            presentations,
        })
    }

    pub fn configured() -> io::Result<Option<Self>> {
        let Some(directory) = std::env::var_os("HL_GPU_CAPTURE_DIR").map(PathBuf::from) else {
            return Ok(None);
        };
        Self::new(
            directory,
            Self::environment_number("HL_GPU_CAPTURE_BATCHES", 64)?,
            Self::environment_number("HL_GPU_CAPTURE_BYTES", 256 << 20)?,
            Self::environment_number("HL_GPU_CAPTURE_PRESENTS", 1)?,
        )
        .map(Some)
    }

    pub fn from_worker(
        arguments: impl IntoIterator<Item = String>,
    ) -> Result<Option<Self>, String> {
        let mut arguments = arguments.into_iter();
        let Some(first) = arguments.next() else {
            return Ok(None);
        };
        let mut values = std::collections::BTreeMap::new();
        let mut flag = first;
        loop {
            let value = arguments
                .next()
                .ok_or_else(|| format!("GPU capture argument {flag} requires a value"))?;
            if values.insert(flag.clone(), value).is_some() {
                return Err(format!("GPU capture argument {flag} was provided twice"));
            }
            let Some(next) = arguments.next() else {
                break;
            };
            flag = next;
        }
        let take = |flag: &str| {
            values
                .get(flag)
                .cloned()
                .ok_or_else(|| format!("GPU capture requires {flag}"))
        };
        let directory = PathBuf::from(take("--gpu-capture-dir")?);
        let number = |flag: &str| {
            take(flag)?
                .parse()
                .map_err(|_| format!("GPU capture argument {flag} must be an unsigned integer"))
        };
        if values.len() != 4 {
            return Err("GPU capture received an unexpected argument".to_owned());
        }
        Self::new(
            directory,
            number("--gpu-capture-batches")?,
            number("--gpu-capture-bytes")?,
            number("--gpu-capture-presents")?,
        )
        .map(Some)
        .map_err(|error| error.to_string())
    }

    pub fn worker_arguments(&self) -> io::Result<Vec<String>> {
        let directory = self.directory.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "GPU capture path is not valid UTF-8",
            )
        })?;
        Ok(vec![
            "--gpu-capture-dir".to_owned(),
            directory.to_owned(),
            "--gpu-capture-batches".to_owned(),
            self.batches.to_string(),
            "--gpu-capture-bytes".to_owned(),
            self.bytes.to_string(),
            "--gpu-capture-presents".to_owned(),
            self.presentations.to_string(),
        ])
    }

    pub fn apply(&self) {
        std::env::set_var("HL_GPU_CAPTURE_DIR", &self.directory);
        std::env::set_var("HL_GPU_CAPTURE_BATCHES", self.batches.to_string());
        std::env::set_var("HL_GPU_CAPTURE_BYTES", self.bytes.to_string());
        std::env::set_var("HL_GPU_CAPTURE_PRESENTS", self.presentations.to_string());
    }

    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(super) fn batches(&self) -> u64 {
        self.batches
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn presentations(&self) -> u64 {
        self.presentations
    }

    fn environment_number(name: &str, default: u64) -> io::Result<u64> {
        match std::env::var(name) {
            Ok(value) => value.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must be an unsigned integer"),
                )
            }),
            Err(std::env::VarError::NotPresent) => Ok(default),
            Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} is not valid UTF-8"),
            )),
        }
    }
}

/// Product composition of host graphics services and their declarative guest requirements.
pub struct Graphics {
    request: hl_container::DeviceRequest,
    driver_directory: String,
    _service: Option<Service>,
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
        let configuration = Configuration::configured()?;
        let presentation = workspace
            .gui
            .then(crate::runtime::compositor::Presentation::configured)
            .transpose()?;
        let native = presentation == Some(crate::runtime::compositor::Presentation::Native);
        let service = (!native)
            .then(|| {
                Service::start(
                    &socket,
                    Configuration::new(configuration.backend(), configuration.trace()),
                )
            })
            .transpose()?;
        let compositor = workspace
            .gui
            .then(|| {
                let presentation = presentation
                    .ok_or_else(|| io::Error::other("GUI presentation mode was not resolved"))?;
                crate::runtime::compositor::Service::start_configured(
                    &wayland,
                    workspace.storage_dir(&state_root).join("frames"),
                    presentation,
                    native
                        .then(|| {
                            crate::runtime::compositor::NativeGpuConfiguration::new(
                                &socket,
                                configuration.backend(),
                                configuration.trace(),
                            )
                        })
                        .transpose()?,
                )
            })
            .transpose()?;
        let projection = Projection::new(workspace.arch);
        let mut namespace = vec![
            hl_container::device::extension::NamespaceEntry::Socket(
                hl_container::device::extension::SocketEntry {
                    path: "/run/hl-gpu.sock".into(),
                    host: socket,
                },
            ),
            projection.root(),
        ];
        let mut request = hl_container::DeviceRequest {
            environment: BTreeMap::from([(
                "HL_GPU_EXEC".to_owned(),
                "/run/hl-gpu.sock".to_owned(),
            )]),
            ..Default::default()
        };
        if workspace.gui {
            namespace.extend(projection.graphics(&drivers)?);
            namespace.push(hl_container::device::extension::NamespaceEntry::Socket(
                hl_container::device::extension::SocketEntry {
                    path: "/run/wayland-0".into(),
                    host: wayland,
                },
            ));
            request.environment.extend(Projection::guest_environment());
        }
        if let Some(cuda) = &workspace.cuda {
            namespace.extend(projection.accelerator(&drivers));
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
        let symlinks = hl_container::device::extension::Feature::new("symlinks")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let directories = hl_container::device::extension::Feature::new("directories")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let immutable_files = hl_container::device::extension::Feature::new("immutable-files")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        request
            .extensions
            .push(hl_container::device::extension::ExtensionSpec {
                provider,
                version: hl_container::device::Version::new(1, 0),
                required: true,
                required_features: BTreeSet::from([
                    directories,
                    host_bind,
                    immutable_files,
                    sockets,
                    symlinks,
                ]),
                optional_features: BTreeSet::new(),
                config: hl_container::device::extension::ExtensionConfig::empty(
                    "engine.namespace/v1",
                ),
                namespace,
                // No loader name aliases: the engine's alias projection forces every aliased guest
                // path read-only, which is exactly what stops dpkg from owning `libgbm.so.1`.
                rules: Vec::new(),
                services: Vec::new(),
                memory: Vec::new(),
                environment: Vec::new(),
            });
        if workspace.gui {
            RenderNode::install(&mut request)?;
        }
        Ok(Some(Self {
            request,
            driver_directory: projection.directory().to_owned(),
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
        let library_path = Projection::search_path(
            &self.driver_directory,
            context
                .process
                .env
                .get("LD_LIBRARY_PATH")
                .map(String::as_str),
        );
        request
            .environment
            .insert("LD_LIBRARY_PATH".to_owned(), library_path);
        Ok(request)
    }
}
