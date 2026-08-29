use crate::{Error, Result, service::ProcessConfig};
use hl_engine::{
    activation::GuestIsa,
    launcher::{entry::GuestPath, plan::RuntimePlan},
    options::Options,
};

pub(super) struct Spec {
    pub(super) isa: GuestIsa,
    pub(super) plan: RuntimePlan,
    pub(super) domain: hl_engine::Domain,
}

impl TryFrom<&ProcessConfig> for Spec {
    type Error = Error;

    fn try_from(launch: &ProcessConfig) -> Result<Self> {
        let isa = match launch.guest {
            crate::Guest::Aarch64 => GuestIsa::Aarch64,
            crate::Guest::X86_64 => GuestIsa::X86_64,
        };
        let domain = launch.domain.unwrap_or(hl_engine::Domain::new()?);
        Self::stage_working_directory(launch)?;
        let mut options = Options::default();
        Self::process(&mut options, launch, domain)?;
        Self::filesystem(&mut options, launch)?;
        Self::resources(&mut options, launch)?;
        Self::network(&mut options, launch)?;
        Self::flag(&mut options, "HL_C_DIAGNOSTICS", launch.execution.diagnostics())?;
        #[cfg(feature = "native-test-hooks")]
        Self::flag(
            &mut options,
            "HL_TRANSLIT_PCACHE_DROP_RELOCATION_TEST",
            std::env::var_os("HL_TRANSLIT_PCACHE_DROP_RELOCATION_TEST").is_some(),
        )?;
        #[cfg(feature = "native-test-hooks")]
        Self::flag(
            &mut options,
            "HL_TRANSLIT_PERF_FRESH_ROLLOVER_TEST",
            std::env::var_os("HL_TRANSLIT_PERF_FRESH_ROLLOVER_TEST").is_some(),
        )?;
        Self::flag(&mut options, "HL_CHECKPOINT", launch.checkpoint.is_some())?;
        Self::flag(
            &mut options,
            "HL_RESTORE",
            launch.checkpoint.as_ref().is_some_and(
                |role| matches!(role, crate::service::CheckpointRole::Coordinator(checkpoint) if checkpoint.restore),
            ),
        )?;

        let guest_program = Self::guest_program(launch);
        let roots = std::iter::once(launch.rootfs.clone())
            .chain(launch.overlay.iter().map(|overlay| overlay.lower.clone()))
            .collect::<Vec<_>>();
        let executable = GuestPath::host_executable(std::path::Path::new(&guest_program), &roots);
        let arguments = std::iter::once(launch.process.program.as_bytes().to_vec())
            .chain(launch.process.args.iter().map(|argument| argument.as_bytes().to_vec()))
            .collect();
        let environment = launch
            .process
            .env
            .records()
            .into_iter()
            .map(|(name, value)| {
                let mut record = Vec::with_capacity(name.len() + value.len() + 1);
                record.extend_from_slice(name);
                record.push(b'=');
                record.extend_from_slice(value);
                record
            })
            .collect();

        let box_policy = hl_engine::launcher::plan::RuntimeBoxPolicy {
            // Retain the daemon-owned coherence epoch in the typed policy as well as the legacy
            // option. Native execution relies on kernel VFS coherence; translated execution maps
            // this file to invalidate its user-space caches.
            filesystem_generation: Some(launch.filesystem_generation.as_os_str().as_encoded_bytes().to_vec()),
            translation_cache: launch
                .translation_cache
                .as_ref()
                .map(|cache| cache.as_os_str().as_encoded_bytes().to_vec()),
            lower_layers: launch
                .overlay
                .as_ref()
                .map(|overlay| overlay.lower.as_os_str().as_encoded_bytes().to_vec()),
            file_owners: {
                let owners = Self::owner_records(launch);
                (!owners.is_empty()).then(|| owners.into_bytes())
            },
            network_mode: match launch.network_mode {
                crate::NetworkMode::Automatic => 0,
                crate::NetworkMode::Host => 2,
            },
            network_namespace: (launch.network_mode == crate::NetworkMode::Automatic)
                .then(|| launch.network_namespace.as_bytes().to_vec()),
            network_interfaces: launch
                .networks
                .iter()
                .filter(|network| network.bridge.is_some())
                .map(|network| hl_engine::launcher::plan::NetworkInterface {
                    bridge: network.bridge.as_ref().expect("validated bridge").as_bytes().to_vec(),
                    address_ipv4_be: u32::from(network.address.expect("validated address")).to_be(),
                    gateway_ipv4_be: u32::from(network.gateway.expect("validated gateway")).to_be(),
                    prefix: network.prefix.expect("validated prefix"),
                })
                .collect(),
            publish: launch
                .publish
                .iter()
                .map(|rule| hl_engine::config::PortPublication {
                    host_ipv4_be: u32::from(rule.host_ip).to_be(),
                    host_port: rule.host,
                    guest_port: rule.port.guest,
                })
                .collect(),
            ..Default::default()
        };
        Ok(Self {
            isa,
            domain,
            plan: RuntimePlan {
                rootfs: Some(launch.rootfs.as_os_str().as_encoded_bytes().to_vec()),
                executable_host: executable.map(|path| path.as_os_str().as_encoded_bytes().to_vec()),
                arguments,
                environment,
                result_path: None,
                options,
                box_policy,
            },
        })
    }
}

impl Spec {
    const MAX_NETWORK_INTERFACES: usize = 8;
    const DEFAULT_PATH: &'static str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

    /// Resolves a bare initial-process name against the image `PATH` inside the rootfs, the way
    /// `execvp` would; names containing a slash are already guest-absolute or relative.
    fn guest_program(launch: &ProcessConfig) -> String {
        let roots = std::iter::once(launch.rootfs.clone())
            .chain(launch.overlay.iter().map(|overlay| overlay.lower.clone()))
            .collect::<Vec<_>>();
        Self::search_path(
            &launch.process.program,
            launch.process.env.get_text("PATH").unwrap_or(Self::DEFAULT_PATH),
            &roots,
        )
    }

    fn search_path(program: &str, search: &str, roots: &[std::path::PathBuf]) -> String {
        let program = program.to_owned();
        if program.contains('/') {
            return program;
        }
        for directory in search.split(':').filter(|entry| !entry.is_empty()) {
            let candidate = format!("{}/{program}", directory.trim_end_matches('/'));
            if roots
                .iter()
                .any(|root| GuestPath::executable_here(&root.join(candidate.trim_start_matches('/'))))
            {
                return candidate;
            }
        }
        program
    }

    /// Docker creates a `WORKDIR`/`-w` directory that the image does not carry; the guest
    /// working-directory base rejects a missing path, so materialize it before launch.
    fn stage_working_directory(launch: &ProcessConfig) -> Result<()> {
        let relative = launch.process.working_dir.as_os_str().as_encoded_bytes();
        let relative = std::path::Path::new(
            std::str::from_utf8(relative)
                .map_err(|_| Error::InvalidSpec("working directory must be valid UTF-8".into()))?,
        )
        .strip_prefix("/")
        .map_err(|_| Error::InvalidSpec("working directory must be absolute".into()))?;
        if relative.as_os_str().is_empty() || launch.isolation.read_only_root {
            return Ok(());
        }
        let present = std::iter::once(&launch.rootfs)
            .chain(launch.overlay.iter().map(|overlay| &overlay.lower))
            .any(|root| root.join(relative).is_dir());
        if present {
            return Ok(());
        }
        let destination = launch
            .overlay
            .as_ref()
            .map_or(&launch.rootfs, |overlay| &overlay.upper)
            .join(relative);
        std::fs::create_dir_all(&destination)
            .map_err(|error| Error::InvalidSpec(format!("working directory {}: {error}", destination.display())))
    }

    fn set(options: &mut Options, name: &str, value: impl AsRef<[u8]>) -> Result<()> {
        options
            .set_bytes(name, value.as_ref(), true)
            .map_err(|error| Error::InvalidSpec(format!("engine option {name}: {error:?}")))
    }

    fn flag(options: &mut Options, name: &str, enabled: bool) -> Result<()> {
        if enabled {
            Self::set(options, name, b"1")?;
        }
        Ok(())
    }

    fn owner_records(launch: &ProcessConfig) -> String {
        launch
            .owners
            .iter()
            .map(|(path, uid, gid)| format!("{}\t{uid}\t{gid}", path.display()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn process(options: &mut Options, launch: &ProcessConfig, domain: hl_engine::Domain) -> Result<()> {
        Self::set(
            options,
            "HL_CWD",
            launch.process.working_dir.as_os_str().as_encoded_bytes(),
        )?;
        Self::set(
            options,
            "HL_PROCESS_DOMAIN",
            format!("{:016x}{:016x}", domain.identity()[0], domain.identity()[1]),
        )?;
        if let Some(uid) = launch.process.uid {
            Self::set(options, "HL_UID", uid.to_string())?;
        }
        if let Some(gid) = launch.process.gid {
            Self::set(options, "HL_GID", gid.to_string())?;
        }
        if let Some(hostname) = &launch.hostname {
            Self::set(options, "HL_HOSTNAME", hostname)?;
        }
        match launch.isolation.sandbox {
            crate::Sandbox::Disabled => {}
            crate::Sandbox::Enabled => {
                Self::set(options, "HL_UNTRUSTED", b"1")?;
                Self::set(options, "HL_SANDBOX", b"1")?;
            }
            crate::Sandbox::SentryOnly => Self::set(options, "HL_UNTRUSTED", b"1")?,
        }
        // The engine defaults to the container baseline, so only the unconfined choice is sent.
        match launch.isolation.seccomp_baseline {
            crate::SeccompBaseline::Container => {}
            crate::SeccompBaseline::Disabled => Self::set(options, "HL_SECCOMP_BASELINE", b"disabled")?,
        }
        Ok(())
    }

    fn filesystem(options: &mut Options, launch: &ProcessConfig) -> Result<()> {
        Self::flag(options, "HL_ROOTFS_RO", launch.isolation.read_only_root)?;
        Self::set(
            options,
            "HL_FSGEN_FILE",
            launch.filesystem_generation.as_os_str().as_encoded_bytes(),
        )?;
        if let Some(cache) = &launch.translation_cache {
            Self::set(options, "HL_PCACHE", b"1")?;
            Self::set(options, "HL_PCACHE_DIR", cache.as_os_str().as_encoded_bytes())?;
        }
        if let Some(overlay) = &launch.overlay {
            Self::set(options, "HL_LOWER", overlay.lower.as_os_str().as_encoded_bytes())?;
            Self::set(
                options,
                "HL_OVERLAY_UPPER",
                overlay.upper.as_os_str().as_encoded_bytes(),
            )?;
            Self::set(options, "HL_OVERLAY_WORK", overlay.work.as_os_str().as_encoded_bytes())?;
        }
        let owners = Self::owner_records(launch);
        if !owners.is_empty() {
            Self::set(options, "HL_FILE_OWNERS", owners)?;
        }
        let mounts = launch
            .mounts
            .iter()
            .map(|mount| {
                let access = match mount.access {
                    crate::Access::ReadOnly => "ro",
                    crate::Access::ReadWrite => "rw",
                };
                format!("{access}:{}:{}", mount.target.display(), mount.source.display())
            })
            .collect::<Vec<_>>()
            .join(",");
        if !mounts.is_empty() {
            Self::set(options, "HL_VOLUMES", mounts)?;
        }
        Ok(())
    }

    fn resources(options: &mut Options, launch: &ProcessConfig) -> Result<()> {
        for (name, value) in [
            ("HL_MEM_MAX", launch.resources.memory_bytes),
            ("HL_PIDS_MAX", u64::from(launch.resources.process_count)),
            ("HL_CPUS", u64::from(launch.resources.cpu_count)),
        ] {
            if value != 0 {
                Self::set(options, name, value.to_string())?;
            }
        }
        if let Some(limit) = launch
            .resources
            .limits
            .iter()
            .find(|limit| !crate::ResourceLimit::NAMES.contains(&limit.name.as_str()))
        {
            return Err(Error::InvalidSpec(format!("unknown resource limit {:?}", limit.name)));
        }
        if !launch.resources.limits.is_empty() {
            let records = launch
                .resources
                .limits
                .iter()
                .map(crate::ResourceLimit::record)
                .collect::<Vec<_>>()
                .join(",");
            Self::set(options, "HL_ULIMITS", records)?;
        }
        Ok(())
    }

    fn network(options: &mut Options, launch: &ProcessConfig) -> Result<()> {
        match launch.network_mode {
            crate::NetworkMode::Host => Self::set(options, "HL_NET_HOST", b"1")?,
            crate::NetworkMode::Automatic => {
                Self::set(options, "HL_NETNS", &launch.network_namespace)?;
                Self::flag(options, "HL_NET_ISOLATE", launch.isolation.network_isolated)?;
            }
        }
        let interfaces = launch
            .networks
            .iter()
            .filter(|network| network.bridge.is_some())
            .collect::<Vec<_>>();
        if interfaces.len() > Self::MAX_NETWORK_INTERFACES {
            return Err(Error::InvalidSpec(format!(
                "at most {} virtual network interfaces are supported",
                Self::MAX_NETWORK_INTERFACES
            )));
        }
        let mut records = Vec::with_capacity(interfaces.len());
        for interface in interfaces {
            let bridge = interface.bridge.as_deref().expect("filtered for bridge");
            if bridge.is_empty()
                || bridge.len() > 40
                || !bridge
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            {
                return Err(Error::InvalidSpec(format!(
                    "virtual network bridge {bridge:?} must be 1..=40 ASCII letters, digits, '-' or '_'"
                )));
            }
            let address = interface
                .address
                .ok_or_else(|| Error::InvalidSpec(format!("virtual network bridge {bridge:?} has no IPv4 address")))?;
            if address.is_unspecified() {
                return Err(Error::InvalidSpec(format!(
                    "virtual network bridge {bridge:?} has an unspecified IPv4 address"
                )));
            }
            let prefix = interface
                .prefix
                .ok_or_else(|| Error::InvalidSpec(format!("virtual network bridge {bridge:?} has no IPv4 prefix")))?;
            if prefix > 32 {
                return Err(Error::InvalidSpec(format!(
                    "virtual network bridge {bridge:?} has IPv4 prefix {prefix} greater than 32"
                )));
            }
            let gateway = interface
                .gateway
                .ok_or_else(|| Error::InvalidSpec(format!("virtual network bridge {bridge:?} has no IPv4 gateway")))?;
            if gateway.is_unspecified() {
                return Err(Error::InvalidSpec(format!(
                    "virtual network bridge {bridge:?} has an unspecified IPv4 gateway"
                )));
            }
            records.push(format!("{bridge}={address}/{prefix}"));
        }
        if !records.is_empty() {
            Self::set(options, "HL_NETIFS", records.join("\n"))?;
        }
        let publish = launch
            .publish
            .iter()
            .map(|rule| format!("{}:{}:{}", rule.host_ip, rule.host, rule.port.guest))
            .collect::<Vec<_>>()
            .join(",");
        if !publish.is_empty() {
            Self::set(options, "HL_PUBLISH", publish)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod path_tests {
    use super::{GuestPath, Spec};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn plant(root: &Path, guest: &str) {
        let path = root.join(guest.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\x7fELF").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hl-spec-path-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A bare name resolves to the `PATH` entry that actually holds an executable, skipping earlier
    /// entries that do not.
    #[test]
    fn a_bare_name_resolves_against_the_image_path() {
        let root = scratch("bare");
        plant(&root, "/usr/local/bin/python");
        let resolved = Spec::search_path("python", "/usr/sbin:/usr/local/bin:/bin", std::slice::from_ref(&root));
        assert_eq!(resolved, "/usr/local/bin/python");
        assert!(root.join("usr/local/bin/python").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A name the rootfs cannot supply stays bare so the loader still reports the lookup failure.
    #[test]
    fn an_absent_name_is_left_alone() {
        let root = scratch("absent");
        assert_eq!(
            Spec::search_path("python", "/usr/bin:/bin", std::slice::from_ref(&root)),
            "python"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A non-executable file of the right name never wins the search.
    #[test]
    fn a_non_executable_candidate_is_skipped() {
        let root = scratch("mode");
        let decoy = root.join("usr/bin/node");
        std::fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        std::fs::write(&decoy, b"text").unwrap();
        std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o644)).unwrap();
        plant(&root, "/bin/node");
        assert_eq!(
            Spec::search_path("node", "/usr/bin:/bin", std::slice::from_ref(&root)),
            "/bin/node"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A program only the lower layer supplies still resolves, and the guest path stays rootfs relative.
    #[test]
    fn a_lower_layer_supplies_the_program() {
        let upper = scratch("upper");
        let lower = scratch("lower");
        plant(&lower, "/usr/local/bin/node");
        let resolved = Spec::search_path("node", "/usr/local/bin:/bin", &[upper.clone(), lower.clone()]);
        assert_eq!(resolved, "/usr/local/bin/node");
        assert_eq!(
            GuestPath::host_executable(std::path::Path::new(&resolved), &[upper.clone(), lower.clone()]),
            Some(lower.join("usr/local/bin/node"))
        );
        std::fs::remove_dir_all(&upper).unwrap();
        std::fs::remove_dir_all(&lower).unwrap();
    }

    /// A name that already carries a separator is passed through untouched.
    #[test]
    fn an_explicit_path_is_not_searched() {
        assert_eq!(Spec::search_path("/bin/sh", "/usr/bin", &[]), "/bin/sh");
        assert_eq!(Spec::search_path("./tool", "/usr/bin", &[]), "./tool");
    }
}
