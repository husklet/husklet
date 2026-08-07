//! Node-kind and metadata answers for procfs and sysfs paths.

use hl_descriptor::OfdMetadata;

use super::{
    DEVICES, Error, FILESYSTEMS, KERNEL_COMMAND_LINE, NodeKind, PROCESS_IO, Procfs, RANDOM_POOL, TTY_DRIVERS, model,
};

impl Procfs {
    pub fn kind(&self, path: &[u8], current: u32) -> Result<Option<NodeKind>, Error> {
        let normalized = path.strip_prefix(b"/").unwrap_or(path);
        if normalized == b"proc/self" || normalized == b"proc/thread-self" {
            let identity = self.source.resolve_process(current)?;
            self.source.process(identity)?;
            return Ok(Some(NodeKind::Link));
        }
        let Some((process, thread, node)) = model::Node::parse(path, current) else {
            return Ok(None);
        };
        let identity = (process != 0)
            .then(|| self.source.resolve_process(process))
            .transpose()?;
        self.validate_thread(identity, thread)?;
        let process_identity = || identity.ok_or(Error::NotFound);
        let kind = match node {
            model::Node::CgroupRoot => {
                self.source.cgroup()?;
                NodeKind::Directory
            }
            model::Node::Cgroup(_) => NodeKind::Regular,
            model::Node::Membership => {
                self.membership(process, process_identity()?)?;
                NodeKind::Regular
            }
            model::Node::ProcRoot => {
                self.source.processes()?;
                NodeKind::Directory
            }
            model::Node::ProcessDirectory | model::Node::ThreadDirectory => {
                self.source.process(process_identity()?)?;
                NodeKind::Directory
            }
            model::Node::TaskDirectory => {
                self.source.threads(process_identity()?)?;
                NodeKind::Directory
            }
            model::Node::NamespaceDirectory => {
                self.source.uts(process_identity()?)?;
                self.source.network(process_identity()?)?;
                NodeKind::Directory
            }
            model::Node::NetworkDirectory => {
                self.source.network(process_identity()?)?;
                NodeKind::Directory
            }
            model::Node::NetworkFile(_) | model::Node::InterfaceFile(_) => {
                self.source.network(process_identity()?)?;
                NodeKind::Regular
            }
            model::Node::InterfaceRoot | model::Node::InterfaceDirectory | model::Node::StatisticsDirectory => {
                let network = self.source.network(process_identity()?)?;
                if node != model::Node::InterfaceRoot {
                    network
                        .interface(model::Node::interface_name(path).ok_or(Error::NotFound)?)
                        .ok_or(Error::NotFound)?;
                }
                NodeKind::Directory
            }
            model::Node::Status
            | model::Node::ProcessStat
            | model::Node::Statm
            | model::Node::Io
            | model::Node::Maps
            | model::Node::NumaMaps
            | model::Node::SmapsRollup
            | model::Node::MapFiles
            | model::Node::MapFile(_, _)
            | model::Node::Smaps
            | model::Node::Limits
            | model::Node::Comm
            | model::Node::Cmdline
            | model::Node::Environ
            | model::Node::OomScore
            | model::Node::OomScoreAdj
            | model::Node::OomAdj
            | model::Node::Mounts
            | model::Node::MountInfo
            | model::Node::MountStats => {
                self.source.process(process_identity()?)?;
                if matches!(
                    node,
                    model::Node::Statm
                        | model::Node::Maps
                        | model::Node::NumaMaps
                        | model::Node::SmapsRollup
                        | model::Node::MapFiles
                        | model::Node::MapFile(_, _)
                        | model::Node::Smaps
                ) {
                    self.source.memory(process_identity()?)?;
                }
                if matches!(
                    node,
                    model::Node::Mounts | model::Node::MountInfo | model::Node::MountStats
                ) {
                    self.source.mounts(process_identity()?)?;
                }
                if node == model::Node::MapFiles {
                    NodeKind::Directory
                } else if matches!(node, model::Node::MapFile(_, _)) {
                    NodeKind::Link
                } else {
                    NodeKind::Regular
                }
            }
            model::Node::Root => {
                self.source.root(process_identity()?)?;
                NodeKind::Link
            }
            model::Node::Cwd => {
                self.source.cwd(process_identity()?)?;
                NodeKind::Link
            }
            model::Node::UtsNamespace
            | model::Node::CgroupNamespace
            | model::Node::IpcNamespace
            | model::Node::MountNamespace
            | model::Node::PidNamespace
            | model::Node::TimeNamespace
            | model::Node::UserNamespace => {
                self.source.uts(process_identity()?)?;
                NodeKind::Link
            }
            model::Node::NetworkNamespace => {
                self.source.network(process_identity()?)?;
                NodeKind::Link
            }
            model::Node::Fd | model::Node::FdInfo => {
                self.source.descriptor_numbers(process_identity()?)?;
                NodeKind::Directory
            }
            model::Node::FdLink(number) => {
                self.source.descriptor(process_identity()?, number)?;
                NodeKind::Link
            }
            model::Node::FdInfoFile(number) => {
                self.source.descriptor(process_identity()?, number)?;
                NodeKind::Regular
            }
            model::Node::CpuInfo | model::Node::CpuStat | model::Node::CpuRange => {
                self.source.cpu()?;
                NodeKind::Regular
            }
            model::Node::CpuDirectory => {
                self.source.cpu()?;
                NodeKind::Directory
            }
            model::Node::CpuLeaf(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                NodeKind::Directory
            }
            model::Node::CpuTopology(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                NodeKind::Directory
            }
            model::Node::CpuCoreId(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                NodeKind::Regular
            }
            model::Node::CpuPackageId(number)
            | model::Node::CpuClusterId(number)
            | model::Node::CpuThreadMask(number)
            | model::Node::CpuThreadList(number)
            | model::Node::CpuCoreMask(number)
            | model::Node::CpuCoreList(number)
            | model::Node::CpuPackageMask(number)
            | model::Node::CpuPackageList(number)
            | model::Node::CpuClusterMask(number)
            | model::Node::CpuClusterList(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                NodeKind::Regular
            }
            model::Node::BlockDirectory => NodeKind::Directory,
            model::Node::TtyDirectory => NodeKind::Directory,
            model::Node::TtyDrivers | model::Node::TtyDisciplines => NodeKind::Regular,
            model::Node::BootIdentity
            | model::Node::RandomIdentity
            | model::Node::EntropyAvailable
            | model::Node::RandomPoolSize => NodeKind::Regular,
            model::Node::Sysctl(_) => NodeKind::Regular,
            model::Node::MemInfo | model::Node::Uptime => {
                self.source.system()?;
                NodeKind::Regular
            }
            model::Node::Devices => NodeKind::Regular,
            model::Node::LoadAverage
            | model::Node::KernelCommandLine
            | model::Node::Filesystems
            | model::Node::Version => NodeKind::Regular,
            model::Node::Hostname | model::Node::Domainname => {
                self.source.uts(self.source.resolve_process(current)?)?;
                NodeKind::Regular
            }
        };
        Ok(Some(kind))
    }

    /// Returns the same immutable logical metadata used by an opened procfs OFD.
    pub fn metadata(&self, path: &[u8], current: u32) -> Result<Option<OfdMetadata>, Error> {
        let normalized = path.strip_prefix(b"/").unwrap_or(path);
        if normalized == b"proc/self" || normalized == b"proc/thread-self" {
            let process = self.source.resolve_process(current)?;
            self.source.process(process)?;
            let identity = if normalized == b"proc/self" {
                0x5000_0001
            } else {
                0x5000_0002
            };
            return Ok(Some(model::Node::link_metadata(identity)));
        }
        let Some((process, thread, node)) = model::Node::parse(path, current) else {
            return Ok(None);
        };
        let identity = (process != 0)
            .then(|| self.source.resolve_process(process))
            .transpose()?;
        let thread_identity = match identity {
            Some(process) if thread.is_some() || node == model::Node::Comm => {
                Some(self.source.resolve_thread(process, thread)?)
            }
            _ => None,
        };
        let process_identity = || identity.ok_or(Error::NotFound);
        let size = match node {
            model::Node::CgroupRoot => {
                self.source.cgroup()?;
                0
            }
            model::Node::Cgroup(name) => self.source.cgroup()?.render(name).len() as u64,
            model::Node::Membership => self.membership(process, process_identity()?)?.len() as u64,
            model::Node::ProcRoot => {
                self.source.processes()?;
                0
            }
            model::Node::ProcessDirectory | model::Node::ThreadDirectory => {
                self.source.process(process_identity()?)?;
                0
            }
            model::Node::TaskDirectory => {
                self.source.threads(process_identity()?)?;
                0
            }
            model::Node::NamespaceDirectory => {
                self.source.uts(process_identity()?)?;
                self.source.network(process_identity()?)?;
                0
            }
            model::Node::NetworkDirectory => {
                self.source.network(process_identity()?)?;
                0
            }
            model::Node::NetworkFile(name) => self.source.network(process_identity()?)?.bytes(name).len() as u64,
            model::Node::InterfaceRoot => {
                self.source.network(process_identity()?)?;
                0
            }
            model::Node::InterfaceDirectory | model::Node::StatisticsDirectory => {
                let network = self.source.network(process_identity()?)?;
                network
                    .interface(model::Node::interface_name(path).ok_or(Error::NotFound)?)
                    .ok_or(Error::NotFound)?;
                0
            }
            model::Node::InterfaceFile(attribute) => {
                let network = self.source.network(process_identity()?)?;
                network
                    .interface(model::Node::interface_name(path).ok_or(Error::NotFound)?)
                    .ok_or(Error::NotFound)?
                    .attribute(attribute)
                    .len() as u64
            }
            model::Node::Status => self.source.process(process_identity()?)?.status().len() as u64,
            model::Node::ProcessStat => self.source.stat(process_identity()?)?.bytes().len() as u64,
            model::Node::Statm => self.source.memory(process_identity()?)?.statm().len() as u64,
            model::Node::Io => PROCESS_IO.len() as u64,
            model::Node::Maps => self.source.address_space(process_identity()?)?.maps(false).len() as u64,
            model::Node::NumaMaps => self.source.address_space(process_identity()?)?.numa().len() as u64,
            model::Node::SmapsRollup => self.source.address_space(process_identity()?)?.rollup().len() as u64,
            model::Node::MapFiles => {
                self.source.address_space(process_identity()?)?;
                0
            }
            model::Node::MapFile(start, end) => {
                self.source
                    .address_space(process_identity()?)?
                    .regions
                    .into_iter()
                    .find(|region| {
                        region.start == start && region.end == end && region.inode != 0 && region.path.is_some()
                    })
                    .ok_or(Error::NotFound)?;
                0
            }
            model::Node::Smaps => self.source.address_space(process_identity()?)?.maps(true).len() as u64,
            model::Node::Limits => self.source.process(process_identity()?)?.limits().len() as u64,
            model::Node::Mounts => self.source.mounts(process_identity()?)?.mounts_bytes().len() as u64,
            model::Node::MountInfo => self.source.mounts(process_identity()?)?.bytes().len() as u64,
            model::Node::MountStats => self.source.mounts(process_identity()?)?.stats().len() as u64,
            model::Node::Comm => self
                .source
                .comm(process_identity()?, thread_identity.ok_or(Error::NotFound)?)?
                .len() as u64,
            model::Node::Cmdline => self.source.cmdline(process_identity()?)?.len() as u64,
            model::Node::Environ => self.source.environment(process_identity()?)?.len() as u64,
            model::Node::OomScore | model::Node::OomAdj => 2,
            model::Node::OomScoreAdj => {
                format!("{}\n", self.source.oom_score_adj(process_identity()?, thread_identity)?).len() as u64
            }
            model::Node::Root => {
                self.source.root(process_identity()?)?;
                0
            }
            model::Node::Cwd => {
                self.source.cwd(process_identity()?)?;
                0
            }
            model::Node::UtsNamespace
            | model::Node::CgroupNamespace
            | model::Node::IpcNamespace
            | model::Node::MountNamespace
            | model::Node::PidNamespace
            | model::Node::TimeNamespace
            | model::Node::UserNamespace => {
                self.source.uts(process_identity()?)?;
                0
            }
            model::Node::NetworkNamespace => {
                self.source.network(process_identity()?)?;
                0
            }
            model::Node::Fd | model::Node::FdInfo => {
                self.source.descriptor_numbers(process_identity()?)?;
                0
            }
            model::Node::FdLink(number) => {
                self.source.descriptor(process_identity()?, number)?;
                0
            }
            model::Node::FdInfoFile(number) => self.source.descriptor(process_identity()?, number)?.info().len() as u64,
            model::Node::CpuInfo => self.source.cpu()?.cpuinfo().len() as u64,
            model::Node::CpuStat => self.source.cpu()?.stat(self.source.system()?).len() as u64,
            model::Node::CpuRange => self.source.cpu()?.range_bytes().len() as u64,
            model::Node::CpuDirectory => {
                self.source.cpu()?;
                0
            }
            model::Node::CpuLeaf(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                0
            }
            model::Node::CpuTopology(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                0
            }
            model::Node::CpuCoreId(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                format!("{number}\n").len() as u64
            }
            model::Node::CpuPackageId(number) | model::Node::CpuClusterId(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                2
            }
            model::Node::CpuThreadList(number) | model::Node::CpuCoreList(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                format!("{number}\n").len() as u64
            }
            model::Node::CpuThreadMask(number) | model::Node::CpuCoreMask(number) => {
                let cpu = self.source.cpu()?;
                if number >= cpu.online() {
                    return Err(Error::NotFound);
                }
                cpu.mask(Some(number)).len() as u64 + 1
            }
            model::Node::CpuPackageMask(number) | model::Node::CpuClusterMask(number) => {
                let cpu = self.source.cpu()?;
                if number >= cpu.online() {
                    return Err(Error::NotFound);
                }
                cpu.mask(None).len() as u64 + 1
            }
            model::Node::CpuPackageList(number) | model::Node::CpuClusterList(number) => {
                let online = self.source.cpu()?.online();
                if number >= online {
                    return Err(Error::NotFound);
                }
                if online == 1 {
                    2
                } else {
                    format!("0-{}\n", online - 1).len() as u64
                }
            }
            model::Node::BlockDirectory => 0,
            model::Node::TtyDirectory => 0,
            model::Node::TtyDrivers => TTY_DRIVERS.len() as u64,
            model::Node::TtyDisciplines => 0,
            model::Node::BootIdentity | model::Node::RandomIdentity => 37,
            model::Node::EntropyAvailable | model::Node::RandomPoolSize => RANDOM_POOL.len() as u64,
            model::Node::Sysctl(bytes) => bytes.len() as u64,
            model::Node::MemInfo => self.source.system()?.meminfo().len() as u64,
            model::Node::Devices => DEVICES.len() as u64,
            model::Node::Uptime => self.source.system()?.uptime().len() as u64,
            model::Node::LoadAverage => {
                let total = self.source.processes()?.len().max(1);
                format!("0.00 0.00 0.00 1/{total} {current}\n").len() as u64
            }
            model::Node::KernelCommandLine => KERNEL_COMMAND_LINE.len() as u64,
            model::Node::Filesystems => FILESYSTEMS.len() as u64,
            model::Node::Version => self.source.cpu()?.version().len() as u64,
            model::Node::Hostname | model::Node::Domainname => {
                let uts = self.source.uts(self.source.resolve_process(current)?)?;
                (1 + if node == model::Node::Hostname {
                    uts.hostname.len()
                } else {
                    uts.domainname.len()
                }) as u64
            }
        };
        let mut metadata = node.metadata(process, size);
        if node == model::Node::UtsNamespace {
            metadata.inode = self.source.uts(process_identity()?)?.namespace;
        } else if node == model::Node::NetworkNamespace {
            metadata.inode = self.source.network(process_identity()?)?.generation;
        } else if let Some((_, inode)) = node.static_namespace() {
            self.source.process(process_identity()?)?;
            metadata.inode = inode;
        }
        Ok(Some(metadata))
    }
}
