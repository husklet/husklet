use hl_isa::GuestArchitecture;
use hl_vfs::{FilesystemKind, FilesystemStats};

pub const STATFS_SIZE: usize = 120;

pub struct StatfsEncoder;

impl StatfsEncoder {
    pub fn encode(architecture: GuestArchitecture, stats: FilesystemStats, output: &mut [u8]) -> Result<(), ()> {
        if output.len() < STATFS_SIZE || stats.validate().is_err() {
            return Err(());
        }
        match architecture {
            GuestArchitecture::Aarch64 | GuestArchitecture::X86_64 => {}
        }
        output[..STATFS_SIZE].fill(0);
        let values = [
            Self::magic(stats.kind),
            stats.block_size,
            stats.blocks,
            stats.blocks_free,
            stats.blocks_available,
            stats.files,
            stats.files_free,
        ];
        for (index, value) in values.into_iter().enumerate() {
            Self::u64(output, index * 8, value);
        }
        Self::u32(output, 56, stats.filesystem_id[0]);
        Self::u32(output, 60, stats.filesystem_id[1]);
        Self::u64(output, 64, stats.name_maximum);
        Self::u64(output, 72, stats.fragment_size);
        Self::u64(output, 80, Self::flags(stats));
        Ok(())
    }

    const fn magic(kind: FilesystemKind) -> u64 {
        match kind {
            FilesystemKind::Overlay => 0x794c_7630,
            FilesystemKind::Proc => 0x9fa0,
            FilesystemKind::Sys => 0x6265_6572,
            FilesystemKind::Cgroup2 => 0x6367_7270,
            FilesystemKind::Tmpfs => 0x0102_1994,
            FilesystemKind::Devpts => 0x1cd1,
            FilesystemKind::Mqueue => 0x1980_0202,
        }
    }

    const fn flags(stats: FilesystemStats) -> u64 {
        (stats.read_only as u64)
            | ((stats.nosuid as u64) << 1)
            | ((stats.nodev as u64) << 2)
            | ((stats.noexec as u64) << 3)
            | 0x20
            | ((stats.relatime as u64) << 12)
    }

    fn u64(output: &mut [u8], offset: usize, value: u64) {
        output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn u32(output: &mut [u8], offset: usize, value: u32) {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
