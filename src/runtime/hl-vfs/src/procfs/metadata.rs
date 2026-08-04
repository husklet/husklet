use hl_descriptor::{OfdMetadata, OfdTimestamp};

use super::model::Node;

impl Node {
    pub(super) fn link_metadata(inode: u64) -> OfdMetadata {
        let mut metadata = Self::ProcRoot.metadata(0, 0);
        metadata.inode = inode;
        metadata.kind = 10;
        metadata.permissions = 0o777;
        metadata
    }

    pub(super) fn metadata(self, process: u32, size: u64) -> OfdMetadata {
        let identity = match self {
            Self::CgroupRoot => 0x6000_0000,
            Self::Cgroup(name) => 0x6100_0000 + name as u64,
            Self::Membership => 0x6200_0000,
            Self::ProcRoot => 0,
            Self::ProcessDirectory => 0x10,
            Self::TaskDirectory => 0x11,
            Self::ThreadDirectory => 0x12,
            Self::NamespaceDirectory => 0x18,
            Self::NetworkDirectory => 0x1b,
            Self::NetworkFile(name) => 0x1c + name as u64,
            Self::InterfaceRoot => 0x3000_0010,
            Self::InterfaceDirectory => 0x3000_0011,
            Self::StatisticsDirectory => 0x3000_0012,
            Self::InterfaceFile(name) => 0x3000_0020 + name.identity(),
            Self::UtsNamespace => 0x19,
            Self::NetworkNamespace => 0x23,
            Self::CgroupNamespace => 0x28,
            Self::IpcNamespace => 0x29,
            Self::MountNamespace => 0x2a,
            Self::PidNamespace => 0x2b,
            Self::TimeNamespace => 0x2c,
            Self::UserNamespace => 0x2d,
            Self::Comm => 0x13,
            Self::Cmdline => 0x1a,
            Self::Environ => 0x24,
            Self::OomScore => 0x25,
            Self::OomScoreAdj => 0x26,
            Self::OomAdj => 0x27,
            Self::Status => 1,
            Self::ProcessStat => 0x14,
            Self::Statm => 0x17,
            Self::Io => 0x20,
            Self::Maps => 0x1d,
            Self::NumaMaps => 0x30,
            Self::SmapsRollup => 0x31,
            Self::MapFiles => 0x1f,
            Self::MapFile(start, end) => 0x5000_0000 ^ start.rotate_left(17) ^ end,
            Self::Smaps => 0x1e,
            Self::Limits => 2,
            Self::Mounts => 0x16,
            Self::MountInfo => 0x15,
            Self::MountStats => 0x32,
            Self::Root => 5,
            Self::Cwd => 6,
            Self::Fd => 3,
            Self::FdInfo => 4,
            Self::FdLink(number) => 0x1000_0000_u64 + number as u64,
            Self::FdInfoFile(number) => 0x2000_0000_u64 + number as u64,
            Self::CpuInfo => 0x3000_0001,
            Self::CpuStat => 0x3000_0002,
            Self::CpuDirectory => 0x3000_0003,
            Self::CpuRange => 0x3000_0004,
            Self::CpuLeaf(number) => 0x4000_0000_u64 + number as u64,
            Self::CpuTopology(number) => 0x4100_0000_u64 + number as u64,
            Self::CpuCoreId(number) => 0x4200_0000_u64 + number as u64,
            Self::CpuPackageId(number) => 0x4300_0000_u64 + number as u64,
            Self::CpuClusterId(number) => 0x4400_0000_u64 + number as u64,
            Self::CpuThreadMask(number) => 0x4500_0000_u64 + number as u64,
            Self::CpuThreadList(number) => 0x4600_0000_u64 + number as u64,
            Self::CpuCoreMask(number) => 0x4700_0000_u64 + number as u64,
            Self::CpuCoreList(number) => 0x4800_0000_u64 + number as u64,
            Self::CpuPackageMask(number) => 0x4900_0000_u64 + number as u64,
            Self::CpuPackageList(number) => 0x4a00_0000_u64 + number as u64,
            Self::CpuClusterMask(number) => 0x4b00_0000_u64 + number as u64,
            Self::CpuClusterList(number) => 0x4c00_0000_u64 + number as u64,
            Self::BlockDirectory => 0x3000_000b,
            Self::TtyDirectory => 0x3000_0013,
            Self::TtyDrivers => 0x3000_0014,
            Self::TtyDisciplines => 0x3000_0015,
            Self::BootIdentity => 0x3000_0016,
            Self::RandomIdentity => 0x3000_0017,
            Self::EntropyAvailable => 0x3000_0018,
            Self::RandomPoolSize => 0x3000_0019,
            Self::Sysctl(bytes) => 0x3001_0000 + bytes.len() as u64,
            Self::MemInfo => 0x3000_0006,
            Self::Devices => 0x3000_000a,
            Self::Uptime => 0x3000_0007,
            Self::LoadAverage => 0x3000_000c,
            Self::KernelCommandLine => 0x3000_000d,
            Self::Filesystems => 0x3000_000e,
            Self::Version => 0x3000_000f,
            Self::Hostname => 0x3000_0008,
            Self::Domainname => 0x3000_0009,
        };
        let directory = matches!(
            self,
            Self::ProcRoot
                | Self::CgroupRoot
                | Self::ProcessDirectory
                | Self::TaskDirectory
                | Self::ThreadDirectory
                | Self::NamespaceDirectory
                | Self::NetworkDirectory
                | Self::InterfaceRoot
                | Self::InterfaceDirectory
                | Self::StatisticsDirectory
                | Self::Fd
                | Self::FdInfo
                | Self::MapFiles
                | Self::CpuDirectory
                | Self::CpuLeaf(_)
                | Self::CpuTopology(_)
                | Self::BlockDirectory
                | Self::TtyDirectory
        );
        let link = matches!(
            self,
            Self::Root
                | Self::Cwd
                | Self::UtsNamespace
                | Self::NetworkNamespace
                | Self::FdLink(_)
                | Self::MapFile(_, _)
        );
        let (kind, permissions) = if directory {
            (4, 0o555)
        } else if link {
            (10, 0o777)
        } else if matches!(self, Self::Comm | Self::OomScoreAdj | Self::Hostname | Self::Domainname) {
            (8, 0o644)
        } else {
            (8, 0o444)
        };
        let zero = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        OfdMetadata {
            device: 0x7072_6f63,
            inode: (u64::from(process) << 32) | identity,
            kind,
            permissions,
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size,
            blocks_512: size.div_ceil(512),
            accessed: zero,
            modified: zero,
            changed: zero,
        }
    }
}
