use hl_isa::GuestArchitecture;
use hl_vfs::{DeviceId, FileKind, FileMetadata, FileTimestamp};

pub const STATX_SIZE: usize = 256;
pub const STATX_BASIC_STATS: u32 = 0x07ff;
pub const STATX_BTIME: u32 = 0x0800;
pub const STATX_MNT_ID: u32 = 0x1000;

const BLOCK_SIZE: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatEncodingError {
    OutputTooSmall,
    InvalidTimestamp,
    Overflow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StatxExtensions {
    pub birth: Option<FileTimestamp>,
    pub mount_id: Option<u64>,
}

pub struct StatEncoder;

impl StatEncoder {
    pub fn encode_stat(
        architecture: GuestArchitecture,
        metadata: &FileMetadata,
        output: &mut [u8],
    ) -> Result<usize, StatEncodingError> {
        let required = architecture.linux_stat_size();
        if output.len() < required {
            return Err(StatEncodingError::OutputTooSmall);
        }
        Self::validate(metadata, architecture, true)?;
        output[..required].fill(0);
        match architecture {
            GuestArchitecture::Aarch64 => {
                Self::encode_aarch64(metadata, output);
            }
            GuestArchitecture::X86_64 => {
                Self::encode_x86_64(metadata, output);
            }
        }
        Ok(required)
    }

    pub fn encode_stat64(
        architecture: GuestArchitecture,
        metadata: &FileMetadata,
        output: &mut [u8],
    ) -> Result<usize, StatEncodingError> {
        Self::encode_stat(architecture, metadata, output)
    }

    pub fn encode_statx(
        metadata: &FileMetadata,
        extensions: StatxExtensions,
        output: &mut [u8],
    ) -> Result<usize, StatEncodingError> {
        if output.len() < STATX_SIZE {
            return Err(StatEncodingError::OutputTooSmall);
        }
        Self::validate(metadata, GuestArchitecture::Aarch64, false)?;
        if let Some(birth) = extensions.birth {
            Self::validate_timestamp(birth)?;
        }
        let links = u32::try_from(metadata.links).map_err(|_| StatEncodingError::Overflow)?;
        output[..STATX_SIZE].fill(0);
        let mut mask = STATX_BASIC_STATS;
        if extensions.birth.is_some() {
            mask |= STATX_BTIME;
        }
        if extensions.mount_id.is_some() {
            mask |= STATX_MNT_ID;
        }
        Self::u32(output, 0, mask);
        Self::u32(output, 4, BLOCK_SIZE as u32);
        Self::u32(output, 16, links);
        Self::u32(output, 20, metadata.user);
        Self::u32(output, 24, metadata.group);
        Self::u16(output, 28, Self::mode(metadata) as u16);
        Self::u64(output, 32, metadata.identity.inode);
        Self::u64(output, 40, metadata.size);
        Self::u64(output, 48, metadata.blocks_512);
        Self::timestamp(output, 64, metadata.accessed);
        if let Some(birth) = extensions.birth {
            Self::timestamp(output, 80, birth);
        }
        Self::timestamp(output, 96, metadata.changed);
        Self::timestamp(output, 112, metadata.modified);
        let special = DeviceId::from_linux_encoded(metadata.special_device);
        let filesystem = DeviceId::from_linux_encoded(metadata.identity.device);
        Self::u32(output, 128, special.major);
        Self::u32(output, 132, special.minor);
        Self::u32(output, 136, filesystem.major);
        Self::u32(output, 140, filesystem.minor);
        if let Some(mount_id) = extensions.mount_id {
            Self::u64(output, 144, mount_id);
        }
        Ok(STATX_SIZE)
    }

    fn validate(
        metadata: &FileMetadata,
        architecture: GuestArchitecture,
        stat_links: bool,
    ) -> Result<(), StatEncodingError> {
        for timestamp in [metadata.accessed, metadata.modified, metadata.changed] {
            Self::validate_timestamp(timestamp)?;
        }
        if metadata.size > i64::MAX as u64 || metadata.blocks_512 > i64::MAX as u64 {
            return Err(StatEncodingError::Overflow);
        }
        if stat_links && architecture == GuestArchitecture::Aarch64 && metadata.links > u64::from(u32::MAX) {
            return Err(StatEncodingError::Overflow);
        }
        Ok(())
    }

    fn validate_timestamp(timestamp: FileTimestamp) -> Result<(), StatEncodingError> {
        if timestamp.nanoseconds >= 1_000_000_000 {
            Err(StatEncodingError::InvalidTimestamp)
        } else {
            Ok(())
        }
    }

    fn encode_aarch64(metadata: &FileMetadata, output: &mut [u8]) {
        Self::u64(output, 0, metadata.identity.device);
        Self::u64(output, 8, metadata.identity.inode);
        Self::u32(output, 16, Self::mode(metadata));
        Self::u32(output, 20, metadata.links as u32);
        Self::u32(output, 24, metadata.user);
        Self::u32(output, 28, metadata.group);
        Self::u64(output, 32, metadata.special_device);
        Self::u64(output, 48, metadata.size);
        Self::u32(output, 56, BLOCK_SIZE as u32);
        Self::u64(output, 64, metadata.blocks_512);
        Self::stat_timestamps(metadata, output);
    }

    fn encode_x86_64(metadata: &FileMetadata, output: &mut [u8]) {
        Self::u64(output, 0, metadata.identity.device);
        Self::u64(output, 8, metadata.identity.inode);
        Self::u64(output, 16, metadata.links);
        Self::u32(output, 24, Self::mode(metadata));
        Self::u32(output, 28, metadata.user);
        Self::u32(output, 32, metadata.group);
        Self::u64(output, 40, metadata.special_device);
        Self::u64(output, 48, metadata.size);
        Self::u64(output, 56, BLOCK_SIZE);
        Self::u64(output, 64, metadata.blocks_512);
        Self::stat_timestamps(metadata, output);
    }

    fn stat_timestamps(metadata: &FileMetadata, output: &mut [u8]) {
        Self::timestamp(output, 72, metadata.accessed);
        Self::timestamp(output, 88, metadata.modified);
        Self::timestamp(output, 104, metadata.changed);
    }

    fn timestamp(output: &mut [u8], offset: usize, value: FileTimestamp) {
        Self::u64(output, offset, value.seconds as u64);
        Self::u64(output, offset + 8, u64::from(value.nanoseconds));
    }

    fn mode(metadata: &FileMetadata) -> u32 {
        let kind = match metadata.kind {
            FileKind::Fifo => 0o010000,
            FileKind::Character => 0o020000,
            FileKind::Directory => 0o040000,
            FileKind::Block => 0o060000,
            FileKind::Regular => 0o100000,
            FileKind::Symlink => 0o120000,
            FileKind::Socket => 0o140000,
        };
        kind | u32::from(metadata.permissions.bits())
    }

    fn u16(output: &mut [u8], offset: usize, value: u16) {
        output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn u32(output: &mut [u8], offset: usize, value: u32) {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn u64(output: &mut [u8], offset: usize, value: u64) {
        output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
