//! Procfs path taxonomy and the parser that resolves paths to nodes.

use crate::procfs::cgroup;

use super::{InterfaceAttribute, NetworkLeaf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Directory,
    Regular,
    Link,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::procfs) enum Node {
    ProcRoot,
    ProcessDirectory,
    TaskDirectory,
    ThreadDirectory,
    NamespaceDirectory,
    CgroupNamespace,
    IpcNamespace,
    MountNamespace,
    NetworkNamespace,
    PidNamespace,
    TimeNamespace,
    UserNamespace,
    NetworkDirectory,
    NetworkFile(NetworkLeaf),
    InterfaceRoot,
    InterfaceDirectory,
    StatisticsDirectory,
    InterfaceFile(InterfaceAttribute),
    UtsNamespace,
    Comm,
    Cmdline,
    Environ,
    OomScore,
    OomScoreAdj,
    OomAdj,
    Status,
    ProcessStat,
    Statm,
    Io,
    Maps,
    NumaMaps,
    SmapsRollup,
    MapFiles,
    MapFile(u64, u64),
    Smaps,
    Limits,
    Mounts,
    MountInfo,
    MountStats,
    Root,
    Cwd,
    Fd,
    FdInfo,
    FdLink(i32),
    FdInfoFile(i32),
    CpuInfo,
    CpuStat,
    CpuDirectory,
    CpuRange,
    CpuLeaf(usize),
    CpuTopology(usize),
    CpuCoreId(usize),
    CpuPackageId(usize),
    CpuClusterId(usize),
    CpuThreadMask(usize),
    CpuThreadList(usize),
    CpuCoreMask(usize),
    CpuCoreList(usize),
    CpuPackageMask(usize),
    CpuPackageList(usize),
    CpuClusterMask(usize),
    CpuClusterList(usize),
    BlockDirectory,
    TtyDirectory,
    TtyDrivers,
    TtyDisciplines,
    BootIdentity,
    RandomIdentity,
    EntropyAvailable,
    RandomPoolSize,
    Sysctl(&'static [u8]),
    MemInfo,
    Devices,
    Uptime,
    LoadAverage,
    KernelCommandLine,
    Filesystems,
    Version,
    Hostname,
    Domainname,
    CgroupRoot,
    Cgroup(cgroup::Leaf),
    Membership,
}

impl Node {
    pub(in crate::procfs) fn static_namespace(self) -> Option<(&'static str, u64)> {
        match self {
            Self::CgroupNamespace => Some(("cgroup", 4_026_531_835)),
            Self::IpcNamespace => Some(("ipc", 4_026_531_839)),
            Self::MountNamespace => Some(("mnt", 4_026_531_841)),
            Self::PidNamespace => Some(("pid", 4_026_531_836)),
            Self::TimeNamespace => Some(("time", 4_026_531_834)),
            Self::UserNamespace => Some(("user", 4_026_531_837)),
            _ => None,
        }
    }

    pub(in crate::procfs) fn interface_name(path: &[u8]) -> Option<&[u8]> {
        let path = path
            .strip_prefix(b"/")
            .unwrap_or(path)
            .strip_prefix(b"sys/class/net/")?;
        path.split(|byte| *byte == b'/').next()
    }

    pub(in crate::procfs) fn entries(self) -> Option<&'static [(&'static [u8], u8)]> {
        const PROCESS: &[(&[u8], u8)] = &[
            (b"cmdline", 8),
            (b"comm", 8),
            (b"cwd", 10),
            (b"fd", 4),
            (b"fdinfo", 4),
            (b"environ", 8),
            (b"oom_score", 8),
            (b"oom_score_adj", 8),
            (b"oom_adj", 8),
            (b"io", 8),
            (b"limits", 8),
            (b"maps", 8),
            (b"numa_maps", 8),
            (b"smaps_rollup", 8),
            (b"map_files", 4),
            (b"mounts", 8),
            (b"mountinfo", 8),
            (b"mountstats", 8),
            (b"pagemap", 8),
            (b"ns", 4),
            (b"root", 10),
            (b"status", 8),
            (b"stat", 8),
            (b"statm", 8),
            (b"smaps", 8),
            (b"task", 4),
            (b"net", 4),
        ];
        const THREAD: &[(&[u8], u8)] = &[
            (b"comm", 8),
            (b"cwd", 10),
            (b"fd", 4),
            (b"fdinfo", 4),
            (b"environ", 8),
            (b"oom_score", 8),
            (b"oom_score_adj", 8),
            (b"oom_adj", 8),
            (b"io", 8),
            (b"limits", 8),
            (b"maps", 8),
            (b"numa_maps", 8),
            (b"smaps_rollup", 8),
            (b"map_files", 4),
            (b"mounts", 8),
            (b"mountinfo", 8),
            (b"mountstats", 8),
            (b"pagemap", 8),
            (b"ns", 4),
            (b"root", 10),
            (b"status", 8),
            (b"stat", 8),
            (b"statm", 8),
            (b"smaps", 8),
        ];
        match self {
            Self::ProcessDirectory => Some(PROCESS),
            Self::ThreadDirectory => Some(THREAD),
            _ => None,
        }
    }

    pub(in crate::procfs) fn parse(path: &[u8], current: u32) -> Option<(u32, Option<u32>, Self)> {
        let path = path.strip_prefix(b"/").unwrap_or(path);
        if path == b"dev/fd" || path == b"dev/fd/" {
            return Some((current, None, Self::Fd));
        }
        if let Some(number) = path.strip_prefix(b"dev/fd/") {
            return Some((current, None, Self::FdLink(Self::number(number)?)));
        }
        if path == b"proc/mounts" {
            return Some((current, None, Self::Mounts));
        }
        if path == b"proc/net" || path == b"proc/net/" {
            return Some((current, None, Self::NetworkDirectory));
        }
        if let Some(name) = path.strip_prefix(b"proc/net/") {
            return Some((current, None, Self::NetworkFile(NetworkLeaf::parse(name)?)));
        }
        if path == b"sys/class/net" || path == b"sys/class/net/" {
            return Some((current, None, Self::InterfaceRoot));
        }
        if let Some(tail) = path.strip_prefix(b"sys/class/net/") {
            let slash = tail.iter().position(|byte| *byte == b'/');
            let (name, leaf) = slash.map_or((tail, &b""[..]), |slash| (&tail[..slash], &tail[slash + 1..]));
            if name.is_empty() {
                return None;
            }
            return Some((
                current,
                None,
                match leaf {
                    b"" => Self::InterfaceDirectory,
                    b"statistics" | b"statistics/" => Self::StatisticsDirectory,
                    _ => Self::InterfaceFile(InterfaceAttribute::parse(leaf)?),
                },
            ));
        }
        if path == b"sys/fs/cgroup" || path == b"sys/fs/cgroup/" {
            return Some((0, None, Self::CgroupRoot));
        }
        if let Some(name) = path.strip_prefix(b"sys/fs/cgroup/") {
            return Some((0, None, Self::Cgroup(cgroup::Leaf::parse(name)?)));
        }
        let global = match path {
            b"proc" | b"proc/" => Some(Self::ProcRoot),
            b"proc/cpuinfo" => Some(Self::CpuInfo),
            b"proc/stat" => Some(Self::CpuStat),
            b"proc/meminfo" => Some(Self::MemInfo),
            b"proc/devices" => Some(Self::Devices),
            b"proc/uptime" => Some(Self::Uptime),
            b"proc/loadavg" => Some(Self::LoadAverage),
            b"proc/cmdline" => Some(Self::KernelCommandLine),
            b"proc/filesystems" => Some(Self::Filesystems),
            b"proc/version" => Some(Self::Version),
            b"proc/tty" | b"proc/tty/" => Some(Self::TtyDirectory),
            b"proc/tty/drivers" => Some(Self::TtyDrivers),
            b"proc/tty/ldiscs" => Some(Self::TtyDisciplines),
            b"proc/sys/kernel/random/boot_id" => Some(Self::BootIdentity),
            b"proc/sys/kernel/random/uuid" => Some(Self::RandomIdentity),
            b"proc/sys/kernel/random/entropy_avail" => Some(Self::EntropyAvailable),
            b"proc/sys/kernel/random/poolsize" => Some(Self::RandomPoolSize),
            b"proc/sys/kernel/ostype" => Some(Self::Sysctl(b"Linux\n")),
            b"proc/sys/kernel/osrelease" => Some(Self::Sysctl(b"6.1.0\n")),
            b"proc/sys/kernel/pid_max" => Some(Self::Sysctl(b"4194304\n")),
            b"proc/sys/kernel/cap_last_cap" => Some(Self::Sysctl(b"40\n")),
            b"proc/sys/kernel/threads-max" => Some(Self::Sysctl(b"63488\n")),
            b"proc/sys/kernel/ngroups_max" => Some(Self::Sysctl(b"65536\n")),
            b"proc/sys/kernel/overflowuid" => Some(Self::Sysctl(b"65534\n")),
            b"proc/sys/kernel/overflowgid" => Some(Self::Sysctl(b"65534\n")),
            b"proc/sys/kernel/randomize_va_space" => Some(Self::Sysctl(b"2\n")),
            b"proc/sys/kernel/shmmax" | b"proc/sys/kernel/shmall" => Some(Self::Sysctl(b"18446744073692774399\n")),
            b"proc/sys/kernel/shmmni" => Some(Self::Sysctl(b"4096\n")),
            b"proc/sys/kernel/sem" => Some(Self::Sysctl(b"256\t131072\t500\t512\n")),
            b"proc/sys/vm/max_map_count" => Some(Self::Sysctl(b"1048576\n")),
            b"proc/sys/vm/mmap_min_addr" => Some(Self::Sysctl(b"32768\n")),
            b"proc/sys/vm/overcommit_memory" => Some(Self::Sysctl(b"1\n")),
            b"proc/sys/vm/swappiness" => Some(Self::Sysctl(b"20\n")),
            b"proc/sys/net/core/somaxconn" => Some(Self::Sysctl(b"4096\n")),
            b"proc/sys/net/core/rmem_max" | b"proc/sys/net/core/wmem_max" => Some(Self::Sysctl(b"7500000\n")),
            b"proc/sys/net/ipv4/ip_local_port_range" => Some(Self::Sysctl(b"32768\t60999\n")),
            b"proc/sys/net/ipv4/tcp_keepalive_time" => Some(Self::Sysctl(b"7200\n")),
            b"proc/sys/net/ipv4/tcp_max_syn_backlog" => Some(Self::Sysctl(b"1024\n")),
            b"proc/sys/net/ipv4/tcp_congestion_control" => Some(Self::Sysctl(b"cubic\n")),
            b"proc/sys/fs/file-max" => Some(Self::Sysctl(b"9223372036854775807\n")),
            b"proc/sys/fs/nr_open" => Some(Self::Sysctl(b"2147483584\n")),
            b"proc/sys/fs/aio-max-nr" => Some(Self::Sysctl(b"1048576\n")),
            b"proc/sys/fs/inotify/max_user_watches" => Some(Self::Sysctl(b"524288\n")),
            b"proc/sys/fs/inotify/max_user_instances" => Some(Self::Sysctl(b"524288\n")),
            b"proc/sys/fs/inotify/max_queued_events" => Some(Self::Sysctl(b"1048576\n")),
            b"proc/sys/fs/mqueue/msg_max" => Some(Self::Sysctl(b"10\n")),
            b"proc/sys/fs/mqueue/msgsize_max" => Some(Self::Sysctl(b"8192\n")),
            b"proc/sys/fs/mqueue/queues_max" => Some(Self::Sysctl(b"256\n")),
            b"sys/kernel/mm/transparent_hugepage/enabled" => Some(Self::Sysctl(b"always [madvise] never\n")),
            b"proc/sys/kernel/hostname" => Some(Self::Hostname),
            b"proc/sys/kernel/domainname" => Some(Self::Domainname),
            b"sys/devices/system/cpu" | b"sys/devices/system/cpu/" => Some(Self::CpuDirectory),
            b"sys/class/block" | b"sys/class/block/" | b"sys/block" | b"sys/block/" => Some(Self::BlockDirectory),
            b"sys/devices/system/cpu/online"
            | b"sys/devices/system/cpu/possible"
            | b"sys/devices/system/cpu/present" => Some(Self::CpuRange),
            _ => None,
        };
        if let Some(node) = global {
            return Some((0, None, node));
        }
        if let Some(tail) = path.strip_prefix(b"sys/devices/system/cpu/cpu") {
            let separator = tail.iter().position(|byte| *byte == b'/');
            let (number, leaf) = separator.map_or((tail, &b""[..]), |separator| {
                (&tail[..separator], &tail[separator + 1..])
            });
            let number = std::str::from_utf8(number).ok()?.parse().ok()?;
            let node = match leaf {
                b"" => Self::CpuLeaf(number),
                b"topology" | b"topology/" => Self::CpuTopology(number),
                b"topology/core_id" => Self::CpuCoreId(number),
                b"topology/physical_package_id" => Self::CpuPackageId(number),
                b"topology/cluster_id" => Self::CpuClusterId(number),
                b"topology/thread_siblings" => Self::CpuThreadMask(number),
                b"topology/thread_siblings_list" => Self::CpuThreadList(number),
                b"topology/core_cpus" => Self::CpuCoreMask(number),
                b"topology/core_cpus_list" => Self::CpuCoreList(number),
                b"topology/core_siblings" | b"topology/package_cpus" => Self::CpuPackageMask(number),
                b"topology/core_siblings_list" => Self::CpuPackageList(number),
                b"topology/package_cpus_list" => Self::CpuPackageList(number),
                b"topology/cluster_cpus" => Self::CpuClusterMask(number),
                b"topology/cluster_cpus_list" => Self::CpuClusterList(number),
                _ => return None,
            };
            return Some((0, None, node));
        }
        let tail = path.strip_prefix(b"proc/")?;
        let separator = tail.iter().position(|byte| *byte == b'/');
        let (owner, leaf) = separator.map_or((tail, &b""[..]), |separator| {
            (&tail[..separator], &tail[separator + 1..])
        });
        let process = if owner == b"self" {
            current
        } else {
            std::str::from_utf8(owner).ok()?.parse::<u32>().ok()?
        };
        if leaf.is_empty() {
            return Some((process, None, Self::ProcessDirectory));
        }
        if leaf == b"cgroup" {
            return Some((process, None, Self::Membership));
        }
        if leaf == b"task" || leaf == b"task/" {
            return Some((process, None, Self::TaskDirectory));
        }
        if leaf == b"ns" || leaf == b"ns/" {
            return Some((process, None, Self::NamespaceDirectory));
        }
        if leaf == b"net" || leaf == b"net/" {
            return Some((process, None, Self::NetworkDirectory));
        }
        if let Some(name) = leaf.strip_prefix(b"net/") {
            return Some((process, None, Self::NetworkFile(NetworkLeaf::parse(name)?)));
        }
        let (thread, leaf) = if let Some(task) = leaf.strip_prefix(b"task/") {
            let separator = task.iter().position(|byte| *byte == b'/');
            let (thread, leaf) = separator.map_or((task, &b""[..]), |separator| {
                (&task[..separator], &task[separator + 1..])
            });
            let thread = Self::task_number(thread)?;
            if leaf.is_empty() {
                return Some((process, Some(thread), Self::ThreadDirectory));
            }
            (Some(thread), leaf)
        } else {
            (None, leaf)
        };
        let leaf = match leaf {
            b"ns" | b"ns/" => Self::NamespaceDirectory,
            b"comm" => Self::Comm,
            b"cmdline" => Self::Cmdline,
            b"environ" => Self::Environ,
            b"oom_score" => Self::OomScore,
            b"oom_score_adj" => Self::OomScoreAdj,
            b"oom_adj" => Self::OomAdj,
            b"status" => Self::Status,
            b"stat" => Self::ProcessStat,
            b"statm" => Self::Statm,
            b"io" => Self::Io,
            b"maps" => Self::Maps,
            b"numa_maps" => Self::NumaMaps,
            b"smaps_rollup" => Self::SmapsRollup,
            b"map_files" | b"map_files/" => Self::MapFiles,
            b"smaps" => Self::Smaps,
            b"limits" => Self::Limits,
            b"mounts" => Self::Mounts,
            b"mountinfo" => Self::MountInfo,
            b"mountstats" => Self::MountStats,
            b"ns/uts" => Self::UtsNamespace,
            b"ns/net" => Self::NetworkNamespace,
            b"ns/cgroup" => Self::CgroupNamespace,
            b"ns/ipc" => Self::IpcNamespace,
            b"ns/mnt" => Self::MountNamespace,
            b"ns/pid" => Self::PidNamespace,
            b"ns/time" => Self::TimeNamespace,
            b"ns/user" => Self::UserNamespace,
            b"root" => Self::Root,
            b"cwd" => Self::Cwd,
            b"fd" => Self::Fd,
            b"fdinfo" => Self::FdInfo,
            _ if leaf.starts_with(b"fd/") => Self::FdLink(Self::number(&leaf[3..])?),
            _ if leaf.starts_with(b"fdinfo/") => Self::FdInfoFile(Self::number(&leaf[7..])?),
            _ if leaf.starts_with(b"map_files/") => {
                let (start, end) = Self::map_range(&leaf[10..])?;
                Self::MapFile(start, end)
            }
            _ => return None,
        };
        Some((process, thread, leaf))
    }

    fn number(bytes: &[u8]) -> Option<i32> {
        if bytes.is_empty() || bytes.contains(&b'/') {
            return None;
        }
        std::str::from_utf8(bytes)
            .ok()?
            .parse()
            .ok()
            .filter(|number| *number >= 0)
    }

    fn task_number(bytes: &[u8]) -> Option<u32> {
        if bytes.is_empty() || bytes.contains(&b'/') {
            return None;
        }
        std::str::from_utf8(bytes)
            .ok()?
            .parse()
            .ok()
            .filter(|number| *number > 0)
    }

    fn map_range(bytes: &[u8]) -> Option<(u64, u64)> {
        let separator = bytes.iter().position(|byte| *byte == b'-')?;
        let (start, end) = (&bytes[..separator], &bytes[separator + 1..]);
        if start.is_empty() || end.is_empty() || end.contains(&b'/') {
            return None;
        }
        let start = u64::from_str_radix(std::str::from_utf8(start).ok()?, 16).ok()?;
        let end = u64::from_str_radix(std::str::from_utf8(end).ok()?, 16).ok()?;
        (start < end).then_some((start, end))
    }
}
