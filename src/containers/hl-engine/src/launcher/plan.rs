//! Projection from validated application configuration to runtime inputs.

use crate::config::{LaunchConfig, PortPublication};
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
}

/// Compatibility name retained while downstream callers migrate to [`RuntimePlan`].
pub type RuntimeLaunchPlan = RuntimePlan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
    OptionStore,
    PublishTooLarge,
    LowerLayersTooLarge,
}

/// Resolves a host path only when the platform adapter is constructed.
pub trait HostPathAccess {
    type Path;
    type Error;

    fn resolve(&self, encoded: &[u8]) -> Result<Self::Path, Self::Error>;
}

/// Applies guest credentials at the process/platform boundary.
pub trait CredentialAccess {
    type Error;

    fn apply(&self, uid: Option<u32>, gid: Option<u32>) -> Result<(), Self::Error>;
}

/// Acquires a named launch service without a process-global locator.
pub trait ServiceAccess {
    type Service;
    type Error;

    fn acquire(&self, name: &[u8]) -> Result<Self::Service, Self::Error>;
}

impl RuntimePlan {
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

        Ok(Self {
            rootfs: (!config.rootfs.is_empty()).then(|| config.rootfs.clone()),
            executable_host: (!config.executable_host.is_empty()).then(|| config.executable_host.clone()),
            arguments: config.arguments.clone(),
            environment: Vec::new(),
            result_path: (!config.result_path.is_empty()).then(|| config.result_path.clone()),
            options,
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

impl From<OptionError> for PlanError {
    fn from(_: OptionError) -> Self {
        Self::OptionStore
    }
}

#[cfg(test)]
#[path = "plan_test.rs"]
mod tests;
