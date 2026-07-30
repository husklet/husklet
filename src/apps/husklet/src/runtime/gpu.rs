//! GPU driver composition for a workspace launch.
//!
//! Driver crates own guest-library injection and API-specific configuration. This module is the one
//! product-level place that decides which drivers a workspace combines.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod capture;
pub(crate) mod executor;
mod render;
pub mod replay;
#[path = "gpu_service.rs"]
mod service;

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
    library_path: String,
    _service: Option<Service>,
    _compositor: Option<crate::runtime::compositor::Service>,
}

impl Graphics {
    fn vulkan_manifest(arch: hl_ws::Arch) -> &'static str {
        match arch {
            // Chromium's Linux GPU broker permits only a fixed set of distribution manifest
            // basenames after sandbox activation. Manifest names carry no Vulkan vendor
            // semantics; the JSON library path remains the authoritative driver identity.
            hl_ws::Arch::Arm64 => "freedreno_icd.aarch64.json",
            hl_ws::Arch::Amd64 => "intel_icd.x86_64.json",
        }
    }

    fn vulkan_library(arch: hl_ws::Arch) -> &'static str {
        match arch {
            hl_ws::Arch::Arm64 => "libvulkan_freedreno.so",
            hl_ws::Arch::Amd64 => "libvulkan_intel.so",
        }
    }

    fn driver_bindings() -> [(&'static str, &'static str, &'static str); 9] {
        [
            ("gl", "libEGL.so.1", "libEGL.so.1"),
            ("gl", "libEGL.so.1", "libEGL.so"),
            ("gl", "libGLESv2.so.2", "libGLESv2.so.2"),
            ("gl", "libGLESv2.so.2", "libGLESv2.so"),
            ("gl", "libgbm.so.1", "libgbm.so.1"),
            ("gl", "libgbm.so.1", "libgbm.so.1.0.0"),
            ("gl", "libgbm.so.1", "libgbm.so"),
            ("vulkan", "libvk_hl.so.1", "libvk_hl.so.1"),
            ("vulkan", "libvk_hl.so.1", "libvk_hl.so"),
        ]
    }

    fn guest_environment() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned()),
            ("XDG_RUNTIME_DIR".to_owned(), "/run".to_owned()),
            ("XDG_SESSION_TYPE".to_owned(), "wayland".to_owned()),
        ])
    }

    fn driver_namespace(
        drivers: &crate::runtime::drivers::Drivers,
        library: &str,
        arch: hl_ws::Arch,
    ) -> io::Result<Vec<hl_container::device::extension::NamespaceEntry>> {
        let mut namespace = Self::driver_bindings()
            .into_iter()
            .map(|(family, source, target)| {
                let path: PathBuf = format!("{library}/{target}").into();
                let host = drivers.path(family, source);
                Ok(hl_container::device::extension::NamespaceEntry::HostBind(
                    hl_container::device::extension::HostBindEntry {
                        path,
                        host,
                        access: hl_container::device::extension::BindAccess::ReadOnly,
                    },
                ))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let source = fs::read_to_string(drivers.path("vulkan", "icd.json"))?;
        let relative = "\"./libvk_hl.so\"";
        if !source.contains(relative) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Vulkan manifest does not name the packaged driver",
            ));
        }
        let contents = source.replace(relative, &format!("\"{}\"", Self::vulkan_library(arch)));
        let vulkan = drivers.path("vulkan", "libvk_hl.so.1");
        namespace.push(hl_container::device::extension::NamespaceEntry::HostBind(
            hl_container::device::extension::HostBindEntry {
                path: Path::new(library).join(Self::vulkan_library(arch)),
                host: vulkan,
                access: hl_container::device::extension::BindAccess::ReadOnly,
            },
        ));
        namespace.push(hl_container::device::extension::NamespaceEntry::File(
            hl_container::device::extension::FileEntry {
                path: Path::new("/usr/share/vulkan/icd.d").join(Self::vulkan_manifest(arch)),
                metadata: hl_container::device::extension::Metadata {
                    mode: 0o444,
                    uid: 0,
                    gid: 0,
                },
                source: hl_container::device::extension::FileSource::Immutable(Arc::from(
                    contents.into_bytes(),
                )),
            },
        ));
        Ok(namespace)
    }

    fn library_search_path(library: &str, inherited: Option<&str>) -> String {
        inherited
            .filter(|value| !value.is_empty())
            .map_or_else(|| library.to_owned(), |value| format!("{library}:{value}"))
    }

    fn loader_rules(
        drivers: &crate::runtime::drivers::Drivers,
    ) -> Vec<hl_container::device::extension::NamespaceRule> {
        [
            (
                ["libEGL.so", "libEGL.so.1"].as_slice(),
                drivers.path("gl", "libEGL.so.1"),
            ),
            (
                ["libGLESv2.so", "libGLESv2.so.2"].as_slice(),
                drivers.path("gl", "libGLESv2.so.2"),
            ),
            (
                ["libgbm.so", "libgbm.so.1", "libgbm.so.1.0.0"].as_slice(),
                drivers.path("gl", "libgbm.so.1"),
            ),
        ]
        .into_iter()
        .map(|(names, host)| {
            hl_container::device::extension::NamespaceRule::NameBind(
                hl_container::device::extension::NameBindEntry {
                    names: names.iter().map(Into::into).collect(),
                    host,
                },
            )
        })
        .collect()
    }

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
            namespace.extend(Self::driver_namespace(&drivers, library, workspace.arch)?);
            namespace.push(hl_container::device::extension::NamespaceEntry::Socket(
                hl_container::device::extension::SocketEntry {
                    path: "/run/wayland-0".into(),
                    host: wayland,
                },
            ));
            request.environment.extend(Self::guest_environment());
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
        let symlinks = hl_container::device::extension::Feature::new("symlinks")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let directories = hl_container::device::extension::Feature::new("directories")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let immutable_files = hl_container::device::extension::Feature::new("immutable-files")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let name_bind = hl_container::device::extension::Feature::new("name-bind-read-only")
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
                    name_bind,
                    sockets,
                    symlinks,
                ]),
                optional_features: BTreeSet::new(),
                config: hl_container::device::extension::ExtensionConfig::empty(
                    "engine.namespace/v1",
                ),
                namespace,
                rules: workspace
                    .gui
                    .then(|| Self::loader_rules(&drivers))
                    .unwrap_or_default(),
                services: Vec::new(),
                memory: Vec::new(),
                environment: Vec::new(),
            });
        if workspace.gui {
            RenderNode::install(&mut request)?;
        }
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
        let library_path = Self::library_search_path(
            &self.library_path,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_vulkan_projection_contract_uses_bundled_artifacts() {
        assert_eq!(
            Graphics::vulkan_manifest(hl_ws::Arch::Arm64),
            "freedreno_icd.aarch64.json"
        );
        assert_eq!(
            Graphics::vulkan_manifest(hl_ws::Arch::Amd64),
            "intel_icd.x86_64.json"
        );
        assert_eq!(
            Graphics::vulkan_library(hl_ws::Arch::Arm64),
            "libvulkan_freedreno.so"
        );
        assert_eq!(
            Graphics::vulkan_library(hl_ws::Arch::Amd64),
            "libvulkan_intel.so"
        );
        let root = tempfile::tempdir().unwrap();
        for (family, name) in [
            ("gl", "libEGL.so.1"),
            ("gl", "libGLESv2.so.2"),
            ("gl", "libwayland-egl.so.1"),
            ("gl", "libgbm.so.1"),
            ("vulkan", "libvk_hl.so.1"),
            ("vulkan", "icd.json"),
        ] {
            let directory = root.path().join(family).join("aarch64");
            fs::create_dir_all(&directory).unwrap();
            let contents = if name == "icd.json" {
                br#"{"ICD":{"library_path":"./libvk_hl.so"}}"#.as_slice()
            } else {
                name.as_bytes()
            };
            fs::write(directory.join(name), contents).unwrap();
        }
        let drivers =
            crate::runtime::drivers::Drivers::open(root.path(), hl_ws::Arch::Arm64, true, false)
                .unwrap();
        let library = "/usr/lib/aarch64-linux-gnu";
        let vulkan = Graphics::driver_bindings()
            .into_iter()
            .filter(|(family, _, _)| *family == "vulkan")
            .map(|(family, source, target)| {
                (
                    Path::new(library).join(target),
                    drivers.path(family, source),
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            vulkan,
            BTreeMap::from([
                (
                    PathBuf::from("/usr/lib/aarch64-linux-gnu/libvk_hl.so"),
                    root.path().join("vulkan/aarch64/libvk_hl.so.1"),
                ),
                (
                    PathBuf::from("/usr/lib/aarch64-linux-gnu/libvk_hl.so.1"),
                    root.path().join("vulkan/aarch64/libvk_hl.so.1"),
                ),
            ])
        );
        assert!(vulkan.values().all(|source| source.is_file()));

        assert!(!Graphics::guest_environment().contains_key("VK_ICD_FILENAMES"));
        let namespace = Graphics::driver_namespace(&drivers, library, hl_ws::Arch::Arm64).unwrap();
        let manifest = namespace
            .iter()
            .find(|entry| {
                entry.path() == Path::new("/usr/share/vulkan/icd.d/freedreno_icd.aarch64.json")
            })
            .unwrap();
        let hl_container::device::extension::NamespaceEntry::File(manifest) = manifest else {
            panic!("the Vulkan manifest must be a sandbox-readable immutable file");
        };
        assert_eq!(manifest.metadata.mode, 0o444);
        let hl_container::device::extension::FileSource::Immutable(contents) = &manifest.source
        else {
            unreachable!();
        };
        assert_eq!(
            std::str::from_utf8(contents).unwrap(),
            r#"{"ICD":{"library_path":"libvulkan_freedreno.so"}}"#
        );
        assert!(namespace.iter().any(|entry| {
            entry.path() == Path::new("/usr/lib/aarch64-linux-gnu/libvulkan_freedreno.so")
        }));
        let amd64 =
            Graphics::driver_namespace(&drivers, "/usr/lib/x86_64-linux-gnu", hl_ws::Arch::Amd64)
                .unwrap();
        assert!(amd64.iter().any(|entry| {
            entry.path() == Path::new("/usr/share/vulkan/icd.d/intel_icd.x86_64.json")
        }));
        assert!(amd64.iter().any(|entry| {
            entry.path() == Path::new("/usr/lib/x86_64-linux-gnu/libvulkan_intel.so")
        }));
        assert_eq!(
            Graphics::library_search_path(library, None),
            "/usr/lib/aarch64-linux-gnu"
        );
        assert_eq!(
            Graphics::library_search_path(library, Some("/opt/chrome")),
            "/usr/lib/aarch64-linux-gnu:/opt/chrome"
        );
    }

    #[test]
    fn loader_aliases_are_declarative_and_do_not_scan_guest_layers() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "libEGL.so.1",
            "libGLESv2.so.2",
            "libwayland-egl.so.1",
            "libgbm.so.1",
        ] {
            let directory = root.path().join("gl/aarch64");
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join(name), name).unwrap();
        }
        let vulkan = root.path().join("vulkan/aarch64");
        fs::create_dir_all(&vulkan).unwrap();
        fs::write(vulkan.join("libvk_hl.so.1"), b"vulkan").unwrap();
        fs::write(
            vulkan.join("icd.json"),
            br#"{"ICD":{"library_path":"./libvk_hl.so"}}"#,
        )
        .unwrap();
        let drivers =
            crate::runtime::drivers::Drivers::open(root.path(), hl_ws::Arch::Arm64, true, false)
                .unwrap();
        let rules = Graphics::loader_rules(&drivers);

        assert_eq!(rules.len(), 3);
        let hl_container::device::extension::NamespaceRule::NameBind(egl) = &rules[0];
        assert_eq!(
            egl.names,
            ["libEGL.so", "libEGL.so.1"]
                .into_iter()
                .map(Into::into)
                .collect()
        );
        assert_eq!(egl.host, root.path().join("gl/aarch64/libEGL.so.1"));
    }
}
