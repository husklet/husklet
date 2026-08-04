use crate::{Error, Result, service::ProcessConfig};
use hl_engine::{activation::GuestIsa, launch_plan::RuntimePlan, options::Options};

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
        let mut options = Options::default();
        Self::process(&mut options, launch, domain)?;
        Self::filesystem(&mut options, launch)?;
        Self::resources(&mut options, launch)?;
        Self::network(&mut options, launch)?;
        Self::flag(&mut options, "HL_NATIVE_EXECUTION", launch.execution.is_native())?;
        Self::flag(&mut options, "HL_NATIVE_DIAGNOSTICS", launch.execution.diagnostics())?;

        let executable = launch.rootfs.join(launch.process.program.trim_start_matches('/'));
        let arguments = std::iter::once(launch.process.program.as_bytes().to_vec())
            .chain(launch.process.args.iter().map(|argument| argument.as_bytes().to_vec()))
            .collect();
        let environment = launch
            .process
            .env
            .iter()
            .map(|(name, value)| format!("{name}={value}").into_bytes())
            .collect();

        Ok(Self {
            isa,
            domain,
            plan: RuntimePlan {
                rootfs: Some(launch.rootfs.as_os_str().as_encoded_bytes().to_vec()),
                executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
                arguments,
                environment,
                result_path: None,
                options,
            },
        })
    }
}

impl Spec {
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
            Self::set(options, "HL_OVERLAY_WORK", overlay.work.as_os_str().as_encoded_bytes())?;
        }
        let owners = launch
            .owners
            .iter()
            .map(|(path, uid, gid)| format!("{}\t{uid}\t{gid}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
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
        if let Some(network) = launch.networks.iter().find(|network| network.bridge.is_some()) {
            if let Some(bridge) = &network.bridge {
                Self::set(options, "HL_NETBR", bridge)?;
            }
            if let Some(address) = network.address {
                Self::set(options, "HL_IP", address.to_string())?;
            }
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
