//! Owned application launch configuration.

use crate::domain::Domain;
use crate::launcher::wire::{PublishRule, Wire, WireError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    InvalidArgument,
    AbiMismatch,
    Corrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortPublication {
    pub host_ipv4_be: u32,
    pub host_port: u16,
    pub guest_port: u16,
}

/// Validated, owned launch input. ISA and host stdio remain activation inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchConfig {
    pub memory_limit: u64,
    pub pid_limit: u32,
    pub cpu_limit: u32,
    pub uid: i32,
    pub gid: i32,
    pub process_domain: [u64; 2],
    pub rootfs_read_only: u32,
    pub sandbox: u32,
    pub network_isolated: u32,
    pub publish_external: u32,
    pub translation_cache_disabled: u32,
    pub checkpoint_mode: u32,
    pub checkpoint_policy: u32,
    pub network_transport: u32,
    pub rootfs: Vec<u8>,
    pub overlay_work: Vec<u8>,
    pub hostname: Vec<u8>,
    pub network_namespace: Vec<u8>,
    pub volumes: Vec<u8>,
    pub name_binds: Vec<u8>,
    pub limits: Vec<u8>,
    pub working_directory: Vec<u8>,
    pub environment: Vec<u8>,
    pub translation_cache: Vec<u8>,
    pub network_bridge: Vec<u8>,
    pub ip: Vec<u8>,
    pub filesystem_generation: Vec<u8>,
    pub egress_proxy: Vec<u8>,
    pub debug_log: Vec<u8>,
    pub result_path: Vec<u8>,
    pub network_interfaces: Vec<u8>,
    pub file_owners: Vec<u8>,
    pub executable_host: Vec<u8>,
    pub lower_layers: Vec<Vec<u8>>,
    pub arguments: Vec<Vec<u8>>,
    pub publish: Vec<PortPublication>,
}

impl LaunchConfig {
    /// Selects the process domain shared by this launch and its descendants.
    #[must_use]
    pub fn domain(mut self, domain: Domain) -> Self {
        self.process_domain = domain.identity();
        self
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        let wire = Wire::parse(bytes).map_err(ConfigError::from)?;
        let header = wire.header();

        // launch.c requires every pooled field to be a terminated string,
        // including fields not yet surfaced by the Rust composition root.
        for offset in [
            header.rootfs_offset,
            header.lower_layers_offset,
            header.overlay_work_offset,
            header.hostname_offset,
            header.network_namespace_offset,
            header.volumes_offset,
            header.limits_offset,
            header.working_directory_offset,
            header.environment_offset,
            header.translation_cache_offset,
            header.network_bridge_offset,
            header.ip_offset,
            header.filesystem_generation_offset,
            header.egress_proxy_offset,
            header.debug_log_offset,
            header.result_path_offset,
            header.network_interfaces_offset,
            header.file_owners_offset,
            header.executable_host_offset,
            header.name_binds_offset,
        ] {
            if offset != 0 {
                wire.string(offset).map_err(ConfigError::from)?;
            }
        }

        let mut lower_layers = Vec::new();
        let mut lower_offset = header.lower_layers_offset;
        for _ in 0..header.lower_layer_count {
            let lower = wire.string(lower_offset).map_err(ConfigError::from)?;
            lower_layers.push(lower.to_vec());
            lower_offset += u32::try_from(lower.len() + 1).map_err(|_| ConfigError::Corrupt)?;
        }
        let borrowed_arguments = wire.arguments().map_err(ConfigError::from)?;
        let arguments = (0..borrowed_arguments.len())
            .map(|index| wire.argument(index).map(<[u8]>::to_vec))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ConfigError::from)?;
        debug_assert_eq!(usize::try_from(header.pool_size).ok(), Some(wire.pool().len()));
        let publish = if header.publish_count == 0 {
            Vec::new()
        } else {
            wire.publish_rules()
                .map_err(ConfigError::from)?
                .into_iter()
                .map(PortPublication::from)
                .collect()
        };

        Ok(Self {
            memory_limit: header.memory_limit,
            pid_limit: header.pid_limit,
            cpu_limit: header.cpu_limit,
            uid: header.uid,
            gid: header.gid,
            process_domain: header.process_domain,
            rootfs_read_only: header.rootfs_read_only,
            sandbox: header.sandbox,
            network_isolated: header.network_isolated,
            publish_external: header.publish_external,
            translation_cache_disabled: header.translation_cache_disabled,
            checkpoint_mode: header.checkpoint_mode,
            checkpoint_policy: header.checkpoint_policy,
            network_transport: header.network_transport,
            rootfs: wire.string(header.rootfs_offset).map_err(ConfigError::from)?.to_vec(),
            overlay_work: wire
                .string(header.overlay_work_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            hostname: wire.string(header.hostname_offset).map_err(ConfigError::from)?.to_vec(),
            network_namespace: wire
                .string(header.network_namespace_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            volumes: wire.string(header.volumes_offset).map_err(ConfigError::from)?.to_vec(),
            name_binds: wire
                .string(header.name_binds_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            limits: wire.string(header.limits_offset).map_err(ConfigError::from)?.to_vec(),
            working_directory: wire
                .string(header.working_directory_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            environment: wire
                .string(header.environment_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            translation_cache: wire
                .string(header.translation_cache_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            network_bridge: wire
                .string(header.network_bridge_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            ip: wire.string(header.ip_offset).map_err(ConfigError::from)?.to_vec(),
            filesystem_generation: wire
                .string(header.filesystem_generation_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            egress_proxy: wire
                .string(header.egress_proxy_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            debug_log: wire
                .string(header.debug_log_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            result_path: wire
                .string(header.result_path_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            network_interfaces: wire
                .string(header.network_interfaces_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            file_owners: wire
                .string(header.file_owners_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            executable_host: wire
                .string(header.executable_host_offset)
                .map_err(ConfigError::from)?
                .to_vec(),
            lower_layers,
            arguments,
            publish,
        })
    }
}

impl From<WireError> for ConfigError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::InvalidArgument => Self::InvalidArgument,
            WireError::AbiMismatch => Self::AbiMismatch,
            WireError::NotFound | WireError::Corrupt => Self::Corrupt,
        }
    }
}

impl From<PublishRule> for PortPublication {
    fn from(rule: PublishRule) -> Self {
        Self {
            host_ipv4_be: rule.host_ipv4_be,
            host_port: rule.host_port,
            guest_port: rule.guest_port,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activation::ActivationStreams;
    use crate::launch_plan::{ConfigOrigin, DiagnosticsMode, LaunchMaterial};

    fn word(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn minimal_wire() -> Vec<u8> {
        let mut bytes = vec![0; 192 + 8];
        word(&mut bytes, 0, 0x484c_4346);
        word(&mut bytes, 4, 8);
        word(&mut bytes, 8, 192);
        word(&mut bytes, 12, 1);
        word(&mut bytes, 108, 1);
        bytes[152..160].copy_from_slice(&1_u64.to_le_bytes());
        bytes[193..200].copy_from_slice(b"guest\0\0");
        bytes
    }

    #[test]
    fn public_boundary_returns() {
        let bytes = minimal_wire();
        let config = LaunchConfig::parse(&bytes).unwrap();
        assert_eq!(config.process_domain, [1, 0]);
        assert_eq!(config.arguments, [b"guest".to_vec()]);
        drop(bytes);
        assert_eq!(config.arguments[0], b"guest");
    }

    #[test]
    fn public_boundary_preserves() {
        let mut bytes = vec![0; 183];
        assert_eq!(LaunchConfig::parse(&bytes), Err(ConfigError::InvalidArgument));
        bytes = minimal_wire();
        word(&mut bytes, 0, 0);
        word(&mut bytes, 12, 2);
        assert_eq!(LaunchConfig::parse(&bytes), Err(ConfigError::Corrupt));
        word(&mut bytes, 0, 0x484c_4346);
        assert_eq!(LaunchConfig::parse(&bytes), Err(ConfigError::AbiMismatch));
    }

    #[test]
    fn launch_material_preserves() {
        let mut bytes = minimal_wire();
        bytes.splice(192..192, [0xaa, 0xbb, 0xcc, 0xdd]);
        word(&mut bytes, 8, 196);
        let material = LaunchMaterial::from_validated_wire(
            &bytes,
            ConfigOrigin::ActivationChannel,
            ActivationStreams::default(),
            None,
            DiagnosticsMode::Disabled,
        )
        .unwrap();
        assert_eq!(material.wire, bytes);
        assert_eq!(material.process_domain, [1, 0]);
    }
}
