//! Borrowed validation and access for the version-one `hl_launch_config` wire image.
//!
//! The retained C ABI is native-endian. Every currently supported host is
//! little-endian, so this parser spells out little-endian decoding and does not
//! rely on a Rust representation or alignment.

pub(crate) const HEADER_SIZE: usize = 192;
pub(crate) const MAGIC: u32 = 0x484c_4346;
pub(crate) const ABI: u32 = 1;

const PUBLISH_RULE_SIZE: usize = 8;
const MAX_LOWER_LAYERS: u32 = 8;
const CHECKPOINT_MODES: u32 = 3;
const MAX_CHECKPOINT_POLICY: u32 = 3;
const NETWORK_ISOLATED: u32 = 1;
const MAX_NETWORK_TRANSPORT: u32 = 2;

/// Status categories returned by the retained C configuration functions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WireError {
    InvalidArgument,
    AbiMismatch,
    NotFound,
    Corrupt,
}

/// The fixed portion of a version-one launch configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub(crate) pool_size: u32,
    pub(crate) header_size: u32,
    pub(crate) memory_limit: u64,
    pub(crate) pid_limit: u32,
    pub(crate) cpu_limit: u32,
    pub(crate) uid: i32,
    pub(crate) gid: i32,
    pub(crate) rootfs_read_only: u32,
    pub(crate) sandbox: u32,
    pub(crate) network_isolated: u32,
    pub(crate) publish_external: u32,
    pub(crate) rootfs_offset: u32,
    pub(crate) lower_layers_offset: u32,
    pub(crate) hostname_offset: u32,
    pub(crate) network_namespace_offset: u32,
    pub(crate) publish_offset: u32,
    pub(crate) volumes_offset: u32,
    pub(crate) limits_offset: u32,
    pub(crate) working_directory_offset: u32,
    pub(crate) environment_offset: u32,
    pub(crate) translation_cache_offset: u32,
    pub(crate) network_bridge_offset: u32,
    pub(crate) ip_offset: u32,
    pub(crate) filesystem_generation_offset: u32,
    pub(crate) arguments_offset: u32,
    pub(crate) translation_cache_disabled: u32,
    pub(crate) egress_proxy_offset: u32,
    pub(crate) debug_log_offset: u32,
    pub(crate) checkpoint_mode: u32,
    pub(crate) checkpoint_policy: u32,
    pub(crate) result_path_offset: u32,
    pub(crate) publish_count: u32,
    pub(crate) network_interfaces_offset: u32,
    pub(crate) file_owners_offset: u32,
    pub(crate) process_domain: [u64; 2],
    pub(crate) executable_host_offset: u32,
    pub(crate) network_transport: u32,
    pub(crate) lower_layer_count: u32,
    pub(crate) overlay_work_offset: u32,
    pub(crate) name_binds_offset: u32,
}

/// A validated header borrowing its string and record pool from the wire image.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wire<'a> {
    header: Header,
    pool: &'a [u8],
}

/// One exact host IPv4 publication, retaining the C field representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PublishRule {
    pub(crate) host_ipv4_be: u32,
    pub(crate) host_port: u16,
    pub(crate) guest_port: u16,
}

impl<'a> Wire<'a> {
    /// Validates in the same order as `hl_launch_config_validate`.
    pub(crate) fn parse(wire: &'a [u8]) -> Result<Self, WireError> {
        let word = Decoder::word;
        if wire.len() < HEADER_SIZE {
            return Err(WireError::InvalidArgument);
        }
        if word(wire, 0) != MAGIC {
            return Err(WireError::Corrupt);
        }
        if word(wire, 12) != ABI {
            return Err(WireError::AbiMismatch);
        }

        let header = Header::decode(wire);
        if header.header_size < HEADER_SIZE as u32
            || word(wire, 148) != 0
            || word(wire, 188) != 0
            || header.checkpoint_policy > MAX_CHECKPOINT_POLICY
            || header.checkpoint_mode & !CHECKPOINT_MODES != 0
            || header.network_transport > MAX_NETWORK_TRANSPORT
            || header.network_isolated != u32::from(header.network_transport == NETWORK_ISOLATED)
            || header.lower_layer_count > MAX_LOWER_LAYERS
            || (header.lower_layer_count == 0) != (header.lower_layers_offset == 0)
            || (header.lower_layer_count == 0) != (header.overlay_work_offset == 0)
        {
            return Err(WireError::Corrupt);
        }
        if header.process_domain[0] | header.process_domain[1] == 0 {
            return Err(WireError::InvalidArgument);
        }

        let header_size = usize::try_from(header.header_size).map_err(|_| WireError::Corrupt)?;
        if header_size > wire.len() {
            return Err(WireError::Corrupt);
        }
        let pool_size = usize::try_from(header.pool_size).map_err(|_| WireError::Corrupt)?;
        let complete_size = header_size.checked_add(pool_size).ok_or(WireError::Corrupt)?;
        if complete_size != wire.len() {
            return Err(WireError::Corrupt);
        }
        let pool = &wire[header_size..];
        if pool.first() != Some(&0) {
            return Err(WireError::Corrupt);
        }
        Header::validate_lowers(&header, pool)?;
        Header::validate_publish_extent(&header, pool)?;
        Ok(Self { header, pool })
    }

    pub(crate) fn header(&self) -> &Header {
        &self.header
    }

    pub(crate) fn pool(&self) -> &'a [u8] {
        self.pool
    }

    /// Implements `hl_launch_config_string`; offset zero names the empty sentinel.
    pub(crate) fn string(&self, offset: u32) -> Result<&'a [u8], WireError> {
        let offset = usize::try_from(offset).map_err(|_| WireError::InvalidArgument)?;
        if offset >= self.pool.len() {
            return Err(WireError::InvalidArgument);
        }
        let tail = &self.pool[offset..];
        let length = tail.iter().position(|byte| *byte == 0).ok_or(WireError::Corrupt)?;
        Ok(&tail[..length])
    }

    /// Implements `hl_launch_config_arguments_validate` and returns all records.
    pub(crate) fn arguments(&self) -> Result<Vec<&'a [u8]>, WireError> {
        let offset = usize::try_from(self.header.arguments_offset).map_err(|_| WireError::InvalidArgument)?;
        if offset == 0 || offset >= self.pool.len() {
            return Err(WireError::InvalidArgument);
        }
        let mut cursor = offset;
        let mut arguments = Vec::new();
        while cursor < self.pool.len() && self.pool[cursor] != 0 {
            let relative = self.pool[cursor..]
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(WireError::Corrupt)?;
            let end = cursor + relative;
            arguments.push(&self.pool[cursor..end]);
            cursor = end + 1;
        }
        if cursor >= self.pool.len() || arguments.is_empty() {
            return Err(WireError::Corrupt);
        }
        Ok(arguments)
    }

    /// Implements `hl_launch_config_argument`, including its error conversion.
    pub(crate) fn argument(&self, index: usize) -> Result<&'a [u8], WireError> {
        self.arguments()
            .map_err(|_| WireError::Corrupt)?
            .get(index)
            .copied()
            .ok_or(WireError::NotFound)
    }

    /// Implements `hl_launch_config_publish`, including nonzero-port validation.
    pub(crate) fn publish_rules(&self) -> Result<Vec<PublishRule>, WireError> {
        if self.header.publish_count == 0 || self.header.publish_offset == 0 {
            return Err(WireError::InvalidArgument);
        }
        let offset = usize::try_from(self.header.publish_offset).map_err(|_| WireError::Corrupt)?;
        let count = usize::try_from(self.header.publish_count).map_err(|_| WireError::Corrupt)?;
        let mut rules = Vec::with_capacity(count);
        for record in self.pool[offset..offset + count * PUBLISH_RULE_SIZE].chunks_exact(PUBLISH_RULE_SIZE) {
            let rule = PublishRule {
                host_ipv4_be: u32::from_le_bytes(record[0..4].try_into().expect("fixed slice")),
                host_port: u16::from_le_bytes(record[4..6].try_into().expect("fixed slice")),
                guest_port: u16::from_le_bytes(record[6..8].try_into().expect("fixed slice")),
            };
            if rule.host_port == 0 || rule.guest_port == 0 {
                return Err(WireError::Corrupt);
            }
            rules.push(rule);
        }
        Ok(rules)
    }
}

impl Header {
    fn decode(wire: &[u8]) -> Self {
        let word = Decoder::word;
        let double_word = Decoder::double_word;
        Self {
            pool_size: word(wire, 4),
            header_size: word(wire, 8),
            memory_limit: double_word(wire, 16),
            pid_limit: word(wire, 24),
            cpu_limit: word(wire, 28),
            uid: word(wire, 32) as i32,
            gid: word(wire, 36) as i32,
            rootfs_read_only: word(wire, 40),
            sandbox: word(wire, 44),
            network_isolated: word(wire, 48),
            publish_external: word(wire, 52),
            rootfs_offset: word(wire, 56),
            lower_layers_offset: word(wire, 60),
            hostname_offset: word(wire, 64),
            network_namespace_offset: word(wire, 68),
            publish_offset: word(wire, 72),
            volumes_offset: word(wire, 76),
            limits_offset: word(wire, 80),
            working_directory_offset: word(wire, 84),
            environment_offset: word(wire, 88),
            translation_cache_offset: word(wire, 92),
            network_bridge_offset: word(wire, 96),
            ip_offset: word(wire, 100),
            filesystem_generation_offset: word(wire, 104),
            arguments_offset: word(wire, 108),
            translation_cache_disabled: word(wire, 112),
            egress_proxy_offset: word(wire, 116),
            debug_log_offset: word(wire, 120),
            checkpoint_mode: word(wire, 124),
            checkpoint_policy: word(wire, 128),
            result_path_offset: word(wire, 132),
            publish_count: word(wire, 136),
            network_interfaces_offset: word(wire, 140),
            file_owners_offset: word(wire, 144),
            process_domain: [double_word(wire, 152), double_word(wire, 160)],
            executable_host_offset: word(wire, 168),
            network_transport: word(wire, 172),
            lower_layer_count: word(wire, 176),
            overlay_work_offset: word(wire, 180),
            name_binds_offset: word(wire, 184),
        }
    }

    fn validate_lowers(header: &Header, pool: &[u8]) -> Result<(), WireError> {
        let mut offset = header.lower_layers_offset;
        for _ in 0..header.lower_layer_count {
            let start = usize::try_from(offset).map_err(|_| WireError::Corrupt)?;
            if start == 0 || start >= pool.len() {
                return Err(WireError::Corrupt);
            }
            let length = pool[start..]
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(WireError::Corrupt)?;
            if length == 0 || pool[start] != b'/' {
                return Err(WireError::Corrupt);
            }
            offset = u32::try_from(start + length + 1).map_err(|_| WireError::Corrupt)?;
        }
        Ok(())
    }

    fn validate_publish_extent(header: &Header, pool: &[u8]) -> Result<(), WireError> {
        if (header.publish_count == 0) != (header.publish_offset == 0) {
            return Err(WireError::Corrupt);
        }
        if header.publish_count == 0 {
            return Ok(());
        }
        let offset = usize::try_from(header.publish_offset).map_err(|_| WireError::Corrupt)?;
        let count = usize::try_from(header.publish_count).map_err(|_| WireError::Corrupt)?;
        let bytes = count.checked_mul(PUBLISH_RULE_SIZE).ok_or(WireError::Corrupt)?;
        if offset % 4 != 0 || offset >= pool.len() || bytes > pool.len() - offset {
            return Err(WireError::Corrupt);
        }
        Ok(())
    }
}

struct Decoder;

impl Decoder {
    fn word(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("validated header"))
    }

    fn double_word(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("validated header"))
    }
}

#[cfg(test)]
#[path = "wire_test.rs"]
mod tests;
