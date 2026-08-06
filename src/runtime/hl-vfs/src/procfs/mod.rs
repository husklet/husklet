//! Typed process-filesystem projection over domain-owned snapshots.

mod cgroup;
mod file;
mod metadata;
mod model;
mod mount;
mod source;
mod stat;

pub use cgroup::View as CgroupView;
pub use model::{
    AddressSpaceView, CpuModel, CpuTicks, CpuView, DescriptorView, InternetSocketView, LimitResource, LimitView,
    MemoryRegionLabel, MemoryRegionView, MemoryView, NetworkInterfaceView, NetworkView, NodeKind, ProcessIdentity,
    ProcessState, ProcessView, SystemView, ThreadIdentity, UnixSocketView, UtsView,
};
pub use mount::{MountEntry, MountView};
pub use source::Source;
pub use stat::{Error as StatError, Input as StatInput, State as StatState, View as StatView};

use std::sync::Arc;

use hl_descriptor::{OfdMetadata, OpenFileDescription};

use crate::OpenIntent;

const PROCESS_IO: &[u8] =
    b"rchar: 0\nwchar: 0\nsyscr: 0\nsyscw: 0\nread_bytes: 0\nwrite_bytes: 0\ncancelled_write_bytes: 0\n";
const FILESYSTEMS: &[u8] = b"nodev\tsysfs\nnodev\ttmpfs\nnodev\tproc\nnodev\tdevtmpfs\nnodev\tdevpts\nnodev\tmqueue\nnodev\tcgroup2\nnodev\toverlay\n\text3\n\text2\n\text4\n";
const KERNEL_COMMAND_LINE: &[u8] = b"root=/dev/sda1 ro quiet\n";
const DEVICES: &[u8] = b"Character devices:\n  1 mem\n  5 /dev/tty\n  5 /dev/console\n  5 /dev/ptmx\n136 pts\n\nBlock devices:\n  7 loop\n  8 sd\n 253 device-mapper\n 259 blkext\n";
const TTY_DRIVERS: &[u8] = b"/dev/tty             /dev/tty        5       0 system:/dev/tty\n/dev/console         /dev/console    5       1 system:console\n/dev/ptmx            /dev/ptmx       5       2 system\nunknown              /dev/tty        4    1-63 console\npty_slave            /dev/pts      136 0-1048575 pty:slave\npty_master           /dev/ptm      128 0-1048575 pty:master\n";
const RANDOM_POOL: &[u8] = b"256\n";

/// Failures exposed while resolving a synthetic process file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    NotFound,
    Access,
    ReadOnly,
    Invalid,
    ResourceLimit,
}

/// Process-filesystem namespace backed by typed domain snapshots.
pub struct Procfs {
    source: Arc<dyn Source>,
}

struct Identity([u8; 16]);

impl Identity {
    fn new(mut bytes: [u8; 16]) -> Self {
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Self(bytes)
    }

    fn into_bytes(self) -> Vec<u8> {
        let bytes = self.0;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\n",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15],
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod identity_test {
    use super::Identity;

    #[test]
    fn wire_contract() {
        let raw = [0xff; 16];
        let first = Identity::new(raw).into_bytes();
        let second = Identity::new(raw).into_bytes();

        assert_eq!(first, b"ffffffff-ffff-4fff-bfff-ffffffffffff\n");
        assert_eq!(second, first);
        assert_eq!(first.len(), 37);
    }
}

impl Procfs {
    #[must_use]
    pub fn new(source: Arc<dyn Source>) -> Self {
        Self { source }
    }

    pub fn open(
        &self,
        path: &[u8],
        current: u32,
        intent: OpenIntent,
    ) -> Result<Option<Arc<dyn OpenFileDescription>>, Error> {
        let writing =
            OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::TRUNCATE | OpenIntent::APPEND | OpenIntent::TEMPORARY;
        let normalized = path.strip_prefix(b"/").unwrap_or(path);
        if intent.bits() & writing != 0 && (normalized == b"sys/fs/cgroup" || normalized.starts_with(b"sys/fs/cgroup/"))
        {
            return Err(Error::ReadOnly);
        }
        if intent.bits() & (OpenIntent::CREATE | OpenIntent::TRUNCATE) != 0 {
            return match model::Node::parse(path, current) {
                Some(_) => Err(Error::Access),
                None => Ok(None),
            };
        }
        let Some((process, thread, leaf)) = model::Node::parse(path, current) else {
            return Ok(None);
        };
        let identity = (process != 0)
            .then(|| self.source.resolve_process(process))
            .transpose()?;
        let thread_identity = match identity {
            Some(process) if thread.is_some() || leaf == model::Node::Comm => {
                Some(self.source.resolve_thread(process, thread)?)
            }
            _ => None,
        };
        if intent.bits() & OpenIntent::WRITE != 0
            && !matches!(
                leaf,
                model::Node::Comm | model::Node::OomScoreAdj | model::Node::Hostname | model::Node::Domainname
            )
        {
            return Err(Error::Access);
        }
        match leaf {
            model::Node::CgroupRoot => {
                self.source.cgroup()?;
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    cgroup::ROOT_NAMES.iter().map(|name| name.to_vec()),
                    leaf.metadata(0, 0),
                ))))
            }
            model::Node::Cgroup(name) => self.snapshot_file(0, leaf, self.source.cgroup()?.render(name)),
            model::Node::Membership => {
                let bytes = self.membership(process, identity.ok_or(Error::NotFound)?)?;
                self.snapshot_file(process, leaf, bytes)
            }
            model::Node::ProcRoot => {
                let mut entries = [
                    (b"cmdline".to_vec(), 8),
                    (b"cpuinfo".to_vec(), 8),
                    (b"filesystems".to_vec(), 8),
                    (b"loadavg".to_vec(), 8),
                    (b"meminfo".to_vec(), 8),
                    (b"mounts".to_vec(), 8),
                    (b"net".to_vec(), 4),
                    (b"self".to_vec(), 10),
                    (b"stat".to_vec(), 8),
                    (b"thread-self".to_vec(), 10),
                    (b"tty".to_vec(), 4),
                    (b"uptime".to_vec(), 8),
                    (b"version".to_vec(), 8),
                ]
                .into_iter()
                .collect::<Vec<_>>();
                entries.extend(
                    self.source
                        .processes()?
                        .into_iter()
                        .map(|process| (process.to_string().into_bytes(), 4)),
                );
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    entries,
                    leaf.metadata(0, 0),
                ))))
            }
            model::Node::ProcessDirectory | model::Node::ThreadDirectory => {
                self.source.process(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    leaf.entries()
                        .into_iter()
                        .flatten()
                        .map(|(name, kind)| (name.to_vec(), *kind)),
                    leaf.metadata(process, 0),
                ))))
            }
            model::Node::TaskDirectory => Ok(Some(Arc::new(file::SnapshotDirectory::names(
                self.source
                    .threads(identity.ok_or(Error::NotFound)?)?
                    .into_iter()
                    .map(|thread| thread.to_string().into_bytes()),
                leaf.metadata(process, 0),
            )))),
            model::Node::NamespaceDirectory => {
                self.source.uts(identity.ok_or(Error::NotFound)?)?;
                self.source.network(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    ["uts", "net", "cgroup", "ipc", "mnt", "pid", "time", "user"]
                        .into_iter()
                        .map(|name| (name.as_bytes().to_vec(), 10)),
                    leaf.metadata(process, 0),
                ))))
            }
            model::Node::NetworkDirectory => {
                let network = self.source.network(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    network.entries().map(|(name, kind)| (name.to_vec(), kind)),
                    leaf.metadata(process, 0),
                ))))
            }
            model::Node::NetworkFile(name) => {
                let bytes = self.source.network(identity.ok_or(Error::NotFound)?)?.bytes(name);
                self.snapshot_file(process, leaf, bytes)
            }
            model::Node::InterfaceRoot => {
                let network = self.source.network(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    network.interfaces.into_iter().map(|interface| interface.name),
                    leaf.metadata(process, 0),
                ))))
            }
            model::Node::InterfaceDirectory => {
                let name = model::Node::interface_name(path).ok_or(Error::NotFound)?;
                self.source
                    .network(identity.ok_or(Error::NotFound)?)?
                    .interface(name)
                    .ok_or(Error::NotFound)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    [
                        (b"address".to_vec(), 8),
                        (b"ifindex".to_vec(), 8),
                        (b"flags".to_vec(), 8),
                        (b"mtu".to_vec(), 8),
                        (b"operstate".to_vec(), 8),
                        (b"statistics".to_vec(), 4),
                        (b"type".to_vec(), 8),
                    ],
                    leaf.metadata(process, 0),
                ))))
            }
            model::Node::StatisticsDirectory => {
                let name = model::Node::interface_name(path).ok_or(Error::NotFound)?;
                self.source
                    .network(identity.ok_or(Error::NotFound)?)?
                    .interface(name)
                    .ok_or(Error::NotFound)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    [b"rx_bytes".to_vec(), b"tx_packets".to_vec()],
                    leaf.metadata(process, 0),
                ))))
            }
            model::Node::InterfaceFile(attribute) => {
                let name = model::Node::interface_name(path).ok_or(Error::NotFound)?;
                let network = self.source.network(identity.ok_or(Error::NotFound)?)?;
                let bytes = network.interface(name).ok_or(Error::NotFound)?.attribute(attribute);
                self.snapshot_file(process, leaf, bytes)
            }
            model::Node::Status => {
                let bytes = self.source.process(identity.ok_or(Error::NotFound)?)?.status();
                let metadata = leaf.metadata(process, bytes.len() as u64);
                Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
            }
            model::Node::ProcessStat => self.snapshot_file(
                process,
                leaf,
                self.source.stat(identity.ok_or(Error::NotFound)?)?.bytes(),
            ),
            model::Node::Statm => self.snapshot_file(
                process,
                leaf,
                self.source.memory(identity.ok_or(Error::NotFound)?)?.statm(),
            ),
            model::Node::Io => self.snapshot_file(process, leaf, PROCESS_IO.to_vec()),
            model::Node::Maps => self.snapshot_file(
                process,
                leaf,
                self.source.address_space(identity.ok_or(Error::NotFound)?)?.maps(false),
            ),
            model::Node::NumaMaps => self.snapshot_file(
                process,
                leaf,
                self.source.address_space(identity.ok_or(Error::NotFound)?)?.numa(),
            ),
            model::Node::SmapsRollup => self.snapshot_file(
                process,
                leaf,
                self.source.address_space(identity.ok_or(Error::NotFound)?)?.rollup(),
            ),
            model::Node::MapFiles => {
                let entries = self
                    .source
                    .address_space(identity.ok_or(Error::NotFound)?)?
                    .regions
                    .into_iter()
                    .filter(|region| region.inode != 0 && region.path.is_some())
                    .map(|region| (format!("{:x}-{:x}", region.start, region.end).into_bytes(), 10));
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    entries,
                    leaf.metadata(process, 0),
                ))))
            }
            model::Node::MapFile(_, _) => Ok(None),
            model::Node::Smaps => self.snapshot_file(
                process,
                leaf,
                self.source.address_space(identity.ok_or(Error::NotFound)?)?.maps(true),
            ),
            model::Node::Comm => {
                let identity = identity.ok_or(Error::NotFound)?;
                let thread_identity = thread_identity.ok_or(Error::NotFound)?;
                let bytes = self.source.comm(identity, thread_identity)?;
                Ok(Some(Arc::new(file::CommFile::new(
                    Arc::clone(&self.source),
                    identity,
                    thread_identity,
                    leaf.metadata(process, bytes.len() as u64),
                ))))
            }
            model::Node::Cmdline => {
                self.snapshot_file(process, leaf, self.source.cmdline(identity.ok_or(Error::NotFound)?)?)
            }
            model::Node::Environ => self.snapshot_file(
                process,
                leaf,
                self.source.environment(identity.ok_or(Error::NotFound)?)?,
            ),
            model::Node::OomScore | model::Node::OomAdj => self.snapshot_file(process, leaf, b"0\n".to_vec()),
            model::Node::OomScoreAdj => Ok(Some(Arc::new(file::OomFile::new(
                Arc::clone(&self.source),
                identity.ok_or(Error::NotFound)?,
                leaf.metadata(process, 0),
            )))),
            model::Node::Limits => {
                let bytes = self.source.process(identity.ok_or(Error::NotFound)?)?.limits();
                let metadata = leaf.metadata(process, bytes.len() as u64);
                Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
            }
            model::Node::Mounts => self.snapshot_file(
                process,
                leaf,
                self.source.mounts(identity.ok_or(Error::NotFound)?)?.mounts_bytes(),
            ),
            model::Node::MountInfo => self.snapshot_file(
                process,
                leaf,
                self.source.mounts(identity.ok_or(Error::NotFound)?)?.bytes(),
            ),
            model::Node::MountStats => self.snapshot_file(
                process,
                leaf,
                self.source.mounts(identity.ok_or(Error::NotFound)?)?.stats(),
            ),
            model::Node::Root | model::Node::Cwd | model::Node::UtsNamespace => Ok(None),
            model::Node::NetworkNamespace
            | model::Node::CgroupNamespace
            | model::Node::IpcNamespace
            | model::Node::MountNamespace
            | model::Node::PidNamespace
            | model::Node::TimeNamespace
            | model::Node::UserNamespace => {
                let metadata = self.namespace_metadata(process, leaf, identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotFile::new(Vec::new(), metadata))))
            }
            model::Node::Fd | model::Node::FdInfo => {
                let file_type = if leaf == model::Node::Fd { 10 } else { 8 };
                Ok(Some(self.source.descriptor_directory(
                    identity.ok_or(Error::NotFound)?,
                    file_type,
                    leaf.metadata(process, 0),
                )?))
            }
            model::Node::FdInfoFile(number) => {
                let descriptor = self.source.descriptor(identity.ok_or(Error::NotFound)?, number)?;
                let bytes = descriptor.info();
                let metadata = leaf.metadata(process, bytes.len() as u64);
                Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
            }
            model::Node::FdLink(_) => Ok(None),
            model::Node::CpuInfo => self.snapshot_file(0, leaf, self.source.cpu()?.cpuinfo()),
            model::Node::CpuStat => self.snapshot_file(0, leaf, self.source.cpu()?.stat(self.source.system()?)),
            model::Node::CpuRange => self.snapshot_file(0, leaf, self.source.cpu()?.range_bytes()),
            model::Node::CpuDirectory => {
                let cpu = self.source.cpu()?;
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    (0..cpu.online()).map(|number| format!("cpu{number}").into_bytes()),
                    leaf.metadata(0, 0),
                ))))
            }
            model::Node::CpuLeaf(number) => {
                let cpu = self.source.cpu()?;
                if number >= cpu.online() {
                    return Err(Error::NotFound);
                }
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    std::iter::empty::<Vec<u8>>(),
                    leaf.metadata(0, 0),
                ))))
            }
            model::Node::CpuTopology(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    [
                        b"core_id".to_vec(),
                        b"physical_package_id".to_vec(),
                        b"cluster_id".to_vec(),
                        b"thread_siblings".to_vec(),
                        b"thread_siblings_list".to_vec(),
                        b"core_siblings".to_vec(),
                        b"core_siblings_list".to_vec(),
                        b"core_cpus".to_vec(),
                        b"core_cpus_list".to_vec(),
                        b"package_cpus".to_vec(),
                        b"package_cpus_list".to_vec(),
                    ],
                    leaf.metadata(0, 0),
                ))))
            }
            model::Node::CpuCoreId(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                self.snapshot_file(0, leaf, format!("{number}\n").into_bytes())
            }
            model::Node::CpuPackageId(number) | model::Node::CpuClusterId(number) => {
                self.cpu_file(number, leaf, b"0\n".to_vec())
            }
            model::Node::CpuThreadMask(number) | model::Node::CpuCoreMask(number) => {
                let cpu = self.source.cpu()?;
                self.cpu_file(number, leaf, format!("{}\n", cpu.mask(Some(number))).into_bytes())
            }
            model::Node::CpuThreadList(number) | model::Node::CpuCoreList(number) => {
                self.cpu_file(number, leaf, format!("{number}\n").into_bytes())
            }
            model::Node::CpuPackageMask(number) | model::Node::CpuClusterMask(number) => {
                let cpu = self.source.cpu()?;
                self.cpu_file(number, leaf, format!("{}\n", cpu.mask(None)).into_bytes())
            }
            model::Node::CpuPackageList(number) | model::Node::CpuClusterList(number) => {
                let cpu = self.source.cpu()?;
                let bytes = if cpu.online() == 1 {
                    b"0\n".to_vec()
                } else {
                    format!("0-{}\n", cpu.online() - 1).into_bytes()
                };
                if number >= cpu.online() {
                    return Err(Error::NotFound);
                }
                self.snapshot_file(0, leaf, bytes)
            }
            model::Node::BlockDirectory => Ok(Some(Arc::new(file::SnapshotDirectory::names(
                std::iter::empty::<Vec<u8>>(),
                leaf.metadata(0, 0),
            )))),
            model::Node::TtyDirectory => Ok(Some(Arc::new(file::SnapshotDirectory::names(
                [b"drivers".to_vec(), b"ldiscs".to_vec()],
                leaf.metadata(0, 0),
            )))),
            model::Node::TtyDrivers => self.snapshot_file(0, leaf, TTY_DRIVERS.to_vec()),
            model::Node::TtyDisciplines => self.snapshot_file(0, leaf, Vec::new()),
            model::Node::BootIdentity => {
                self.snapshot_file(0, leaf, Identity::new(self.source.boot_identity()?).into_bytes())
            }
            model::Node::RandomIdentity => {
                self.snapshot_file(0, leaf, Identity::new(self.source.random_identity()?).into_bytes())
            }
            model::Node::EntropyAvailable | model::Node::RandomPoolSize => {
                self.snapshot_file(0, leaf, RANDOM_POOL.to_vec())
            }
            model::Node::Sysctl(bytes) => self.snapshot_file(0, leaf, bytes.to_vec()),
            model::Node::MemInfo => self.snapshot_file(0, leaf, self.source.system()?.meminfo()),
            model::Node::Devices => self.snapshot_file(0, leaf, DEVICES.to_vec()),
            model::Node::Uptime => self.snapshot_file(0, leaf, self.source.system()?.uptime()),
            model::Node::LoadAverage => {
                let total = self.source.processes()?.len().max(1);
                self.snapshot_file(0, leaf, format!("0.00 0.00 0.00 1/{total} {current}\n").into_bytes())
            }
            model::Node::KernelCommandLine => self.snapshot_file(0, leaf, KERNEL_COMMAND_LINE.to_vec()),
            model::Node::Filesystems => self.snapshot_file(0, leaf, FILESYSTEMS.to_vec()),
            model::Node::Version => self.snapshot_file(0, leaf, self.source.cpu()?.version()),
            model::Node::Hostname | model::Node::Domainname => {
                let uts = self.source.uts(self.source.resolve_process(current)?)?;
                Ok(Some(Arc::new(file::UtsFile::new(
                    Arc::clone(&self.source),
                    uts.namespace,
                    leaf == model::Node::Domainname,
                    leaf.metadata(current, 0),
                ))))
            }
        }
    }

    fn snapshot_file(
        &self,
        process: u32,
        node: model::Node,
        bytes: Vec<u8>,
    ) -> Result<Option<Arc<dyn OpenFileDescription>>, Error> {
        let metadata = node.metadata(process, bytes.len() as u64);
        Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
    }

    fn cpu_file(
        &self,
        number: usize,
        node: model::Node,
        bytes: Vec<u8>,
    ) -> Result<Option<Arc<dyn OpenFileDescription>>, Error> {
        if number >= self.source.cpu()?.online() {
            return Err(Error::NotFound);
        }
        self.snapshot_file(0, node, bytes)
    }

    pub fn read_link(&self, path: &[u8], current: u32) -> Result<Option<Vec<u8>>, Error> {
        self.read_link_for(path, current, current)
    }

    pub fn read_link_for(&self, path: &[u8], current: u32, thread: u32) -> Result<Option<Vec<u8>>, Error> {
        let path = path.strip_prefix(b"/").unwrap_or(path);
        if path == b"proc/self" {
            let identity = self.source.resolve_process(current)?;
            self.source.process(identity)?;
            return Ok(Some(current.to_string().into_bytes()));
        }
        if path == b"proc/thread-self" {
            let identity = self.source.resolve_process(current)?;
            self.source.resolve_thread(identity, Some(thread))?;
            return Ok(Some(format!("{current}/task/{thread}").into_bytes()));
        }
        if let Some((process, thread, model::Node::Root)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            return self.source.root(identity).map(Some);
        }
        if let Some((process, thread, model::Node::Cwd)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            return self.source.cwd(identity).map(Some);
        }
        if let Some((process, thread, model::Node::UtsNamespace)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            let namespace = self.source.uts(identity)?.namespace;
            return Ok(Some(format!("uts:[{namespace}]").into_bytes()));
        }
        if let Some((process, thread, model::Node::NetworkNamespace)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            let namespace = self.source.network(identity)?.generation;
            return Ok(Some(format!("net:[{namespace}]").into_bytes()));
        }
        if let Some((process, thread, node)) = model::Node::parse(path, current)
            && let Some((name, inode)) = node.static_namespace()
        {
            let identity = self.resolve_path_process(process, thread)?;
            self.source.process(identity)?;
            return Ok(Some(format!("{name}:[{inode}]").into_bytes()));
        }
        if let Some((process, thread, model::Node::MapFile(start, end))) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            let target = self
                .source
                .address_space(identity)?
                .regions
                .into_iter()
                .find(|region| region.start == start && region.end == end && region.inode != 0)
                .and_then(|region| region.path)
                .ok_or(Error::NotFound)?;
            return Ok(Some(target));
        }
        let Some((process, thread, model::Node::FdLink(number))) = model::Node::parse(path, current) else {
            return Ok(None);
        };
        let identity = self.resolve_path_process(process, thread)?;
        let target = self
            .source
            .descriptor(identity, number)?
            .target
            .ok_or(Error::NotFound)?;
        Ok(Some(target))
    }

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
            model::Node::OomScoreAdj => format!("{}\n", self.source.oom_score_adj(process_identity()?)?).len() as u64,
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

    fn validate_thread(
        &self,
        process: Option<model::ProcessIdentity>,
        thread: Option<u32>,
    ) -> Result<Option<model::ThreadIdentity>, Error> {
        match (process, thread) {
            (_, None) => Ok(None),
            (Some(process), Some(thread)) => self.source.resolve_thread(process, Some(thread)).map(Some),
            (None, Some(_)) => Err(Error::NotFound),
        }
    }

    fn membership(&self, process: u32, identity: model::ProcessIdentity) -> Result<Vec<u8>, Error> {
        // Pin the numeric projection to the resolved task generation before reading
        // the immutable cgroup snapshot. Unified membership bytes contain no task
        // generation, so the number is only an index after this liveness check.
        self.source.process(identity)?;
        self.source.cgroup()?.membership(process).ok_or(Error::NotFound)
    }

    fn namespace_metadata(
        &self,
        process: u32,
        node: model::Node,
        identity: model::ProcessIdentity,
    ) -> Result<OfdMetadata, Error> {
        let mut metadata = node.metadata(process, 0);
        if node == model::Node::UtsNamespace {
            metadata.inode = self.source.uts(identity)?.namespace;
        } else if node == model::Node::NetworkNamespace {
            metadata.inode = self.source.network(identity)?.generation;
        } else if let Some((_, inode)) = node.static_namespace() {
            self.source.process(identity)?;
            metadata.inode = inode;
        } else {
            return Err(Error::NotFound);
        }
        Ok(metadata)
    }

    fn resolve_path_process(&self, process: u32, thread: Option<u32>) -> Result<model::ProcessIdentity, Error> {
        let identity = self.source.resolve_process(process)?;
        self.validate_thread(Some(identity), thread)?;
        Ok(identity)
    }

    /// Resolves one live UTS namespace magic link to its stable task-owned identity.
    pub fn uts_namespace(&self, path: &[u8], current: u32) -> Result<Option<u64>, Error> {
        let Some((process, thread, model::Node::UtsNamespace)) = model::Node::parse(path, current) else {
            return Ok(None);
        };
        let identity = self.resolve_path_process(process, thread)?;
        self.source.uts(identity).map(|view| Some(view.namespace))
    }

    pub fn namespace_inode(&self, path: &[u8], current: u32) -> Result<Option<u64>, Error> {
        let Some((_, _, node)) = model::Node::parse(path.strip_prefix(b"/").unwrap_or(path), current) else {
            return Ok(None);
        };
        let namespace = matches!(
            node,
            model::Node::UtsNamespace
                | model::Node::NetworkNamespace
                | model::Node::CgroupNamespace
                | model::Node::IpcNamespace
                | model::Node::MountNamespace
                | model::Node::PidNamespace
                | model::Node::TimeNamespace
                | model::Node::UserNamespace
        );
        if !namespace {
            return Ok(None);
        }
        self.metadata(path, current)
            .map(|metadata| metadata.map(|value| value.inode))
    }
}

#[cfg(test)]
mod test;
