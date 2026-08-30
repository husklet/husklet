//! Projection from validated application configuration to runtime inputs, and the host
//! preconditions the projected plan states but cannot itself satisfy.

use crate::config::{LaunchConfig, PortPublication};
#[cfg(unix)]
use crate::engine::EngineError;
use crate::options::{OptionError, Options};

const NETWORK_HOST: u32 = 2;
const SANDBOX_ENABLED: u32 = 1;
const PUBLISH_LIMIT: usize = 1024;
const LOWERS_LIMIT: usize = 8192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsMode {
    Disabled,
    Enabled,
}

#[derive(Clone, Debug)]
pub struct RuntimePlan {
    pub rootfs: Option<Vec<u8>>,
    pub executable_host: Option<Vec<u8>>,
    pub arguments: Vec<Vec<u8>>,
    pub environment: Vec<Vec<u8>>,
    pub result_path: Option<Vec<u8>>,
    pub options: Options,
    /// Owned typed container policy retained beside the legacy option mirror.
    pub box_policy: RuntimeBoxPolicy,
}

/// Owned form of the public `hl_engine_box_config` contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeBoxPolicy {
    pub flags: u32,
    pub uid: i32,
    pub gid: i32,
    pub working_directory: Option<Vec<u8>>,
    pub hostname: Option<Vec<u8>>,
    pub environment: Option<Vec<u8>>,
    pub lower_layers: Option<Vec<u8>>,
    pub publish: Vec<PortPublication>,
    pub volumes: Option<Vec<u8>>,
    pub limits: Option<Vec<u8>>,
    pub network_namespace: Option<Vec<u8>>,
    pub translation_cache: Option<Vec<u8>>,
    pub translation_symbols: Option<Vec<u8>>,
    /// Daemon-authenticated executable identities. This has no public Config projection.
    pub executable_digests: Vec<ExecutableDigestAuthority>,
    pub network_bridge: Option<Vec<u8>>,
    pub ip: Option<Vec<u8>>,
    pub filesystem_generation: Option<Vec<u8>>,
    pub egress_proxy: Option<Vec<u8>>,
    pub file_owners: Option<Vec<u8>>,
    pub checkpoint_mode: u32,
    pub checkpoint_policy: u32,
    pub network_mode: u32,
    pub network_interfaces: Vec<NetworkInterface>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableDigestAuthority {
    pub snapshot: Vec<u8>,
    pub guest_path: Vec<u8>,
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkInterface {
    pub bridge: Vec<u8>,
    pub address_ipv4_be: u32,
    pub gateway_ipv4_be: u32,
    pub prefix: u8,
}

impl Default for RuntimeBoxPolicy {
    fn default() -> Self {
        Self {
            flags: 0,
            uid: -1,
            gid: -1,
            working_directory: None,
            hostname: None,
            environment: None,
            lower_layers: None,
            publish: Vec::new(),
            volumes: None,
            limits: None,
            network_namespace: None,
            translation_cache: None,
            translation_symbols: None,
            executable_digests: Vec::new(),
            network_bridge: None,
            ip: None,
            filesystem_generation: None,
            egress_proxy: None,
            file_owners: None,
            checkpoint_mode: 0,
            checkpoint_policy: 0,
            network_mode: 0,
            network_interfaces: Vec::new(),
        }
    }
}

impl RuntimeBoxPolicy {
    fn project(config: &LaunchConfig, lower_layers: Vec<u8>, network_namespace: Option<Vec<u8>>) -> Self {
        let mut flags = 0;
        flags |= (config.rootfs_read_only != 0) as u32;
        flags |= ((config.sandbox == SANDBOX_ENABLED) as u32) << 1;
        flags |= ((config.network_isolated != 0) as u32) << 2;
        flags |= ((config.publish_external != 0) as u32) << 3;
        flags |= ((config.translation_cache_disabled != 0) as u32) << 4;
        flags |= ((config.sandbox != 0 && config.sandbox != SANDBOX_ENABLED) as u32) << 5;
        Self {
            flags,
            uid: config.uid,
            gid: config.gid,
            working_directory: nonempty(&config.working_directory),
            hostname: nonempty(&config.hostname),
            environment: nonempty(&config.environment),
            lower_layers: nonempty_owned(lower_layers),
            publish: config.publish.clone(),
            volumes: nonempty(&config.volumes),
            limits: nonempty(&config.limits),
            network_namespace,
            translation_cache: nonempty(&config.translation_cache),
            translation_symbols: None,
            executable_digests: Vec::new(),
            network_bridge: nonempty(&config.network_bridge),
            ip: nonempty(&config.ip),
            filesystem_generation: nonempty(&config.filesystem_generation),
            egress_proxy: nonempty(&config.egress_proxy),
            file_owners: nonempty(&config.file_owners),
            checkpoint_mode: config.checkpoint_mode,
            checkpoint_policy: config.checkpoint_policy,
            network_mode: u32::from(config.network_transport == NETWORK_HOST) * NETWORK_HOST,
            network_interfaces: Vec::new(),
        }
    }
}

fn nonempty(value: &[u8]) -> Option<Vec<u8>> {
    (!value.is_empty()).then(|| value.to_vec())
}

fn nonempty_owned(value: Vec<u8>) -> Option<Vec<u8>> {
    (!value.is_empty()).then_some(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
    OptionStore,
    PublishTooLarge,
    LowerLayersTooLarge,
}

impl RuntimePlan {
    /// The writable root this plan will materialize guest state into, if it has one.
    ///
    /// An overlay upper layer, when the launch configures one, is where every guest write lands and the
    /// lower layers are read-only by construction -- so a root-owned lower layer is legitimate and must
    /// not be refused. Without an overlay the rootfs itself is the writable root, unless the launch
    /// asked for `HL_ROOTFS_RO`, in which case nothing is written and any owner works.
    #[cfg(unix)]
    #[must_use]
    pub fn writable_root(&self) -> Option<&[u8]> {
        if let Some(upper) = self.options.get_bytes("HL_OVERLAY_UPPER") {
            return Some(upper);
        }
        if self.options.get_bytes("HL_ROOTFS_RO").is_some() {
            return None;
        }
        self.rootfs.as_deref()
    }

    /// Refuses a launch whose writable root belongs to a host user the engine cannot act as.
    ///
    /// The engine runs as an unprivileged host uid and never acquires host privilege: guest ownership
    /// lives in its own owner overlay (`container/owner.h`, `HL_FILE_OWNERS`), which is why a guest can
    /// report `id -u` = 0 while every write to a host-root-owned tree returns `EACCES`. Granting the
    /// access is not merely refused by the engine, it is unimplementable -- `chmod(2)` refuses for a
    /// non-owner without `CAP_FOWNER` -- so the contract has to be stated at launch instead.
    ///
    /// Only the root directory is examined. Walking the tree would be unbounded work at launch, and a
    /// root-owned subtree below a writable root is a legitimate shape: shared read-only layers and host
    /// bind mounts both produce it, and the guest can still do everything the host user could. A root
    /// the engine does not own is different in kind -- nothing in the workspace is writable, so the
    /// failure is total, and refusing is kinder than letting a developer find it through a failing
    /// `git checkout`.
    ///
    /// A path that cannot be stat'd is not refused here: the existing launch path already owns "the
    /// rootfs is not there", and an ownership cause for a missing directory is a worse diagnostic than
    /// the one it would replace. Running as host root refuses nothing, because then every owner is
    /// writable.
    #[cfg(unix)]
    // The engine's own effective uid is the only thing this needs from the host and libc's
    // `geteuid` is its only spelling; std exposes no safe equivalent. The call takes no argument,
    // reads no caller memory and cannot fail, so the block has nothing else to justify.
    #[allow(unsafe_code)]
    pub(crate) fn refuse_unownable_root(&self) -> Result<(), EngineError> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        let Some(root) = self.writable_root() else {
            return Ok(());
        };
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(root));
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(());
        };
        // SAFETY: `geteuid` takes no arguments, reads no caller memory, and is documented never to fail.
        let engine_uid = unsafe { libc::geteuid() };
        let rootfs_uid = metadata.uid();
        if !root_is_unownable(engine_uid, rootfs_uid) {
            return Ok(());
        }
        hl_log::hl_error!(
            hl_log::tag::EXEC,
            "refusing launch: the writable root {} is owned by host uid {rootfs_uid}, but the engine runs \
             as host uid {engine_uid} and never acquires host privilege, so no guest write can succeed \
             however the guest reports its own id. Re-materialize the rootfs as uid {engine_uid} -- unpack \
             it without sudo, or `chown -R {engine_uid} {}` -- or launch it read-only with HL_ROOTFS_RO.",
            path.display(),
            path.display()
        );
        Err(EngineError::RootfsNotOwnedByEngine { rootfs_uid, engine_uid })
    }

    pub fn project(config: &LaunchConfig, diagnostics: DiagnosticsMode) -> Result<Self, PlanError> {
        let mut options = Options::default();
        Self::set_number(&mut options, "HL_MEM_MAX", config.memory_limit)?;
        Self::set_number(&mut options, "HL_PIDS_MAX", u64::from(config.pid_limit))?;
        Self::set_number(&mut options, "HL_CPUS", u64::from(config.cpu_limit))?;
        Self::set_flag(&mut options, "HL_ROOTFS_RO", config.rootfs_read_only != 0)?;
        Self::set_flag(&mut options, "HL_NET_ISOLATE", config.network_isolated != 0)?;
        Self::set_flag(&mut options, "HL_NET_HOST", config.network_transport == NETWORK_HOST)?;
        Self::set_flag(&mut options, "HL_PUBLISH_DAEMON", config.publish_external != 0)?;
        if config.uid >= 0 {
            Self::set_number(&mut options, "HL_UID", config.uid as u64)?;
        }
        if config.gid >= 0 {
            Self::set_number(&mut options, "HL_GID", config.gid as u64)?;
        }

        let domain = format!("{:016x}{:016x}", config.process_domain[0], config.process_domain[1]);
        options
            .set("HL_PROCESS_DOMAIN", &domain, true)
            .map_err(PlanError::from)?;

        Self::set_value(&mut options, "HL_HOSTNAME", &config.hostname)?;
        Self::set_value(&mut options, "HL_ULIMITS", &config.limits)?;
        let publish = Self::publish_records(&config.publish)?;
        Self::set_value(&mut options, "HL_PUBLISH", &publish)?;
        let lowers = Self::lower_records(&config.lower_layers)?;
        Self::set_value(&mut options, "HL_LOWER", &lowers)?;
        Self::set_value(&mut options, "HL_OVERLAY_WORK", &config.overlay_work)?;
        if config.network_transport != NETWORK_HOST {
            let namespace = if config.network_namespace.is_empty() {
                domain.as_bytes()
            } else {
                &config.network_namespace
            };
            options
                .set_bytes("HL_NETNS", namespace, true)
                .map_err(PlanError::from)?;
        }
        for (name, value) in [
            ("HL_VOLUMES", config.volumes.as_slice()),
            ("HL_NAME_BINDS", config.name_binds.as_slice()),
            ("HL_CWD", config.working_directory.as_slice()),
            ("HL_GUEST_ENV", config.environment.as_slice()),
            ("HL_NETBR", config.network_bridge.as_slice()),
            ("HL_IP", config.ip.as_slice()),
            ("HL_NETIFS", config.network_interfaces.as_slice()),
            ("HL_FILE_OWNERS", config.file_owners.as_slice()),
            ("HL_FSGEN_FILE", config.filesystem_generation.as_slice()),
            ("HL_EGRESS_SOCKS", config.egress_proxy.as_slice()),
        ] {
            Self::set_value(&mut options, name, value)?;
        }
        Self::set_flag(&mut options, "HL_CHECKPOINT", config.checkpoint_mode & 1 != 0)?;
        Self::set_flag(&mut options, "HL_RESTORE", config.checkpoint_mode & 2 != 0)?;
        Self::set_number(
            &mut options,
            "HL_CHECKPOINT_POLICY",
            u64::from(config.checkpoint_policy),
        )?;
        if diagnostics == DiagnosticsMode::Enabled {
            Self::set_value(&mut options, "HL_LOG", &config.debug_log)?;
        }
        if !config.translation_cache.is_empty() {
            options.set("HL_PCACHE", "1", true).map_err(PlanError::from)?;
            options
                .set_bytes("HL_PCACHE_DIR", &config.translation_cache, true)
                .map_err(PlanError::from)?;
        }
        if config.translation_cache_disabled != 0 {
            options.unset("HL_PCACHE").map_err(PlanError::from)?;
            options.unset("HL_PCACHE_DIR").map_err(PlanError::from)?;
        }
        if config.sandbox != 0 {
            options.set("HL_UNTRUSTED", "1", true).map_err(PlanError::from)?;
            if config.sandbox == SANDBOX_ENABLED {
                options.set("HL_SANDBOX", "1", true).map_err(PlanError::from)?;
            }
        }

        let box_policy = RuntimeBoxPolicy::project(config, lowers, options.get_bytes("HL_NETNS").map(<[u8]>::to_vec));
        Ok(Self {
            rootfs: (!config.rootfs.is_empty()).then(|| config.rootfs.clone()),
            executable_host: (!config.executable_host.is_empty()).then(|| config.executable_host.clone()),
            arguments: config.arguments.clone(),
            environment: Vec::new(),
            result_path: (!config.result_path.is_empty()).then(|| config.result_path.clone()),
            options,
            box_policy,
        })
    }

    fn set_number(options: &mut Options, name: &str, value: u64) -> Result<(), PlanError> {
        if value != 0 || name == "HL_CHECKPOINT_POLICY" {
            options.set(name, &value.to_string(), true).map_err(PlanError::from)?;
        }
        Ok(())
    }

    fn set_flag(options: &mut Options, name: &str, enabled: bool) -> Result<(), PlanError> {
        if enabled {
            options.set(name, "1", true).map_err(PlanError::from)?;
        }
        Ok(())
    }

    fn set_value(options: &mut Options, name: &str, value: &[u8]) -> Result<(), PlanError> {
        if !value.is_empty() {
            options.set_bytes(name, value, true).map_err(PlanError::from)?;
        }
        Ok(())
    }

    fn lower_records(layers: &[Vec<u8>]) -> Result<Vec<u8>, PlanError> {
        let mut records = Vec::new();
        for layer in layers {
            if !records.is_empty() {
                records.push(b'\n');
            }
            records.extend_from_slice(layer);
            if records.len() + 1 > LOWERS_LIMIT {
                return Err(PlanError::LowerLayersTooLarge);
            }
        }
        Ok(records)
    }

    fn publish_records(rules: &[PortPublication]) -> Result<Vec<u8>, PlanError> {
        let mut records = String::new();
        for (index, rule) in rules.iter().enumerate() {
            if index != 0 {
                records.push(',');
            }
            if rule.host_ipv4_be == 0 {
                records.push_str(&format!("{}:{}", rule.host_port, rule.guest_port));
            } else {
                let address = rule.host_ipv4_be.to_le_bytes();
                records.push_str(&format!(
                    "{}.{}.{}.{}:{}:{}",
                    address[0], address[1], address[2], address[3], rule.host_port, rule.guest_port
                ));
            }
            if records.len() + 1 > PUBLISH_LIMIT {
                return Err(PlanError::PublishTooLarge);
            }
        }
        Ok(records.into_bytes())
    }
}

#[cfg(unix)]
const fn root_is_unownable(engine_uid: u32, rootfs_uid: u32) -> bool {
    engine_uid != 0 && rootfs_uid != engine_uid
}

impl From<OptionError> for PlanError {
    fn from(_: OptionError) -> Self {
        Self::OptionStore
    }
}

#[cfg(test)]
#[path = "plan_test.rs"]
mod tests;
