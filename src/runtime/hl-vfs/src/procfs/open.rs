//! Descriptor construction for procfs and sysfs paths.

use std::sync::Arc;

use hl_descriptor::OpenFileDescription;

use crate::OpenIntent;

use super::{
    DEVICES, Error, FILESYSTEMS, Identity, KERNEL_COMMAND_LINE, PROCESS_IO, Procfs, RANDOM_POOL, TTY_DRIVERS, cgroup,
    file, metadata, model,
};

impl Procfs {
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
        let context = self.context(identity)?;
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
                    leaf.metadata(0, context),
                ))))
            }
            model::Node::Cgroup(name) => self.snapshot_file(context, 0, leaf, self.source.cgroup()?.render(name)),
            model::Node::Membership => {
                let bytes = self.membership(process, identity.ok_or(Error::NotFound)?)?;
                self.snapshot_file(context, process, leaf, bytes)
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
                    leaf.metadata(0, context),
                ))))
            }
            model::Node::ProcessDirectory | model::Node::ThreadDirectory => {
                self.source.process(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    leaf.entries()
                        .into_iter()
                        .flatten()
                        .map(|(name, kind)| (name.to_vec(), *kind)),
                    leaf.metadata(process, context),
                ))))
            }
            model::Node::TaskDirectory => Ok(Some(Arc::new(file::SnapshotDirectory::names(
                self.source
                    .threads(identity.ok_or(Error::NotFound)?)?
                    .into_iter()
                    .map(|thread| thread.to_string().into_bytes()),
                leaf.metadata(process, context),
            )))),
            model::Node::NamespaceDirectory => {
                self.source.uts(identity.ok_or(Error::NotFound)?)?;
                self.source.network(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    ["uts", "net", "cgroup", "ipc", "mnt", "pid", "time", "user"]
                        .into_iter()
                        .map(|name| (name.as_bytes().to_vec(), 10)),
                    leaf.metadata(process, context),
                ))))
            }
            model::Node::NetworkDirectory => {
                let network = self.source.network(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::entries(
                    network.entries().map(|(name, kind)| (name.to_vec(), kind)),
                    leaf.metadata(process, context),
                ))))
            }
            model::Node::NetworkFile(name) => {
                let bytes = self.source.network(identity.ok_or(Error::NotFound)?)?.bytes(name);
                self.snapshot_file(context, process, leaf, bytes)
            }
            model::Node::InterfaceRoot => {
                let network = self.source.network(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    network.interfaces.into_iter().map(|interface| interface.name),
                    leaf.metadata(process, context),
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
                    leaf.metadata(process, context),
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
                    leaf.metadata(process, context),
                ))))
            }
            model::Node::InterfaceFile(attribute) => {
                let name = model::Node::interface_name(path).ok_or(Error::NotFound)?;
                let network = self.source.network(identity.ok_or(Error::NotFound)?)?;
                let bytes = network.interface(name).ok_or(Error::NotFound)?.attribute(attribute);
                self.snapshot_file(context, process, leaf, bytes)
            }
            model::Node::Status => {
                let bytes = self.source.process(identity.ok_or(Error::NotFound)?)?.status();
                let metadata = leaf.metadata(process, context);
                Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
            }
            model::Node::ProcessStat => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.stat(identity.ok_or(Error::NotFound)?)?.bytes(),
            ),
            model::Node::Statm => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.memory(identity.ok_or(Error::NotFound)?)?.statm(),
            ),
            model::Node::Io => self.snapshot_file(context, process, leaf, PROCESS_IO.to_vec()),
            model::Node::Maps => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.address_space(identity.ok_or(Error::NotFound)?)?.maps(false),
            ),
            model::Node::NumaMaps => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.address_space(identity.ok_or(Error::NotFound)?)?.numa(),
            ),
            model::Node::SmapsRollup => self.snapshot_file(
                context,
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
                    leaf.metadata(process, context),
                ))))
            }
            model::Node::MapFile(_, _) => Ok(None),
            model::Node::Auxv => {
                self.source.process(identity.ok_or(Error::NotFound)?)?;
                Ok(None)
            }
            model::Node::Pagemap => {
                self.source.process(identity.ok_or(Error::NotFound)?)?;
                Ok(Some(Arc::new(file::PagemapFile::new(leaf.metadata(process, context)))))
            }
            model::Node::Smaps => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.address_space(identity.ok_or(Error::NotFound)?)?.maps(true),
            ),
            model::Node::Comm => {
                let identity = identity.ok_or(Error::NotFound)?;
                let thread_identity = thread_identity.ok_or(Error::NotFound)?;
                self.source.comm(identity, thread_identity)?;
                Ok(Some(Arc::new(file::CommFile::new(
                    Arc::clone(&self.source),
                    identity,
                    thread_identity,
                    leaf.metadata(process, context),
                ))))
            }
            model::Node::Cmdline => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.cmdline(identity.ok_or(Error::NotFound)?)?,
            ),
            model::Node::Environ => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.environment(identity.ok_or(Error::NotFound)?)?,
            ),
            model::Node::OomScore | model::Node::OomAdj => self.snapshot_file(context, process, leaf, b"0\n".to_vec()),
            model::Node::OomScoreAdj => {
                let identity = identity.ok_or(Error::NotFound)?;
                let value = self.source.oom_score_adj(identity, thread_identity)?;
                let length = format!("{value}\n").len();
                Ok(Some(Arc::new(file::OomFile::new(
                    Arc::clone(&self.source),
                    identity,
                    thread_identity,
                    length,
                    leaf.metadata(process, context),
                ))))
            }
            model::Node::Limits => {
                let bytes = self.source.process(identity.ok_or(Error::NotFound)?)?.limits();
                let metadata = leaf.metadata(process, context);
                Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
            }
            model::Node::Mounts => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.mounts(identity.ok_or(Error::NotFound)?)?.mounts_bytes(),
            ),
            model::Node::MountInfo => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.mounts(identity.ok_or(Error::NotFound)?)?.bytes(),
            ),
            model::Node::MountStats => self.snapshot_file(
                context,
                process,
                leaf,
                self.source.mounts(identity.ok_or(Error::NotFound)?)?.stats(),
            ),
            model::Node::Root
            | model::Node::Cwd
            | model::Node::Exe
            | model::Node::MountsLink
            | model::Node::UtsNamespace => Ok(None),
            model::Node::NetworkNamespace
            | model::Node::CgroupNamespace
            | model::Node::IpcNamespace
            | model::Node::MountNamespace
            | model::Node::PidNamespace
            | model::Node::TimeNamespace
            | model::Node::UserNamespace => {
                let metadata = self.namespace_metadata(process, leaf, identity.ok_or(Error::NotFound)?)?;
                let metadata = model::Node::namespace_file(metadata);
                Ok(Some(Arc::new(file::SnapshotFile::new(Vec::new(), metadata))))
            }
            model::Node::Fd | model::Node::FdInfo => {
                let file_type = if leaf == model::Node::Fd { 10 } else { 8 };
                Ok(Some(self.source.descriptor_directory(
                    identity.ok_or(Error::NotFound)?,
                    file_type,
                    leaf.metadata(process, context),
                )?))
            }
            model::Node::FdInfoFile(number) => {
                let descriptor = self.source.descriptor(identity.ok_or(Error::NotFound)?, number)?;
                let bytes = descriptor.info();
                let metadata = leaf.metadata(process, context);
                Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
            }
            model::Node::FdLink(_) => Ok(None),
            model::Node::CpuInfo => self.snapshot_file(context, 0, leaf, self.source.cpu()?.cpuinfo()),
            model::Node::CpuStat => {
                self.snapshot_file(context, 0, leaf, self.source.cpu()?.stat(self.source.system()?))
            }
            model::Node::CpuRange => self.snapshot_file(context, 0, leaf, self.source.cpu()?.range_bytes()),
            model::Node::CpuDirectory => {
                let cpu = self.source.cpu()?;
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    (0..cpu.online()).map(|number| format!("cpu{number}").into_bytes()),
                    leaf.metadata(0, context),
                ))))
            }
            model::Node::CpuLeaf(number) => {
                let cpu = self.source.cpu()?;
                if number >= cpu.online() {
                    return Err(Error::NotFound);
                }
                Ok(Some(Arc::new(file::SnapshotDirectory::names(
                    std::iter::empty::<Vec<u8>>(),
                    leaf.metadata(0, context),
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
                    leaf.metadata(0, context),
                ))))
            }
            model::Node::CpuCoreId(number) => {
                if number >= self.source.cpu()?.online() {
                    return Err(Error::NotFound);
                }
                self.snapshot_file(context, 0, leaf, format!("{number}\n").into_bytes())
            }
            model::Node::CpuPackageId(number) | model::Node::CpuClusterId(number) => {
                self.cpu_file(context, number, leaf, b"0\n".to_vec())
            }
            model::Node::CpuThreadMask(number) | model::Node::CpuCoreMask(number) => {
                let cpu = self.source.cpu()?;
                self.cpu_file(
                    context,
                    number,
                    leaf,
                    format!("{}\n", cpu.mask(Some(number))).into_bytes(),
                )
            }
            model::Node::CpuThreadList(number) | model::Node::CpuCoreList(number) => {
                self.cpu_file(context, number, leaf, format!("{number}\n").into_bytes())
            }
            model::Node::CpuPackageMask(number) | model::Node::CpuClusterMask(number) => {
                let cpu = self.source.cpu()?;
                self.cpu_file(context, number, leaf, format!("{}\n", cpu.mask(None)).into_bytes())
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
                self.snapshot_file(context, 0, leaf, bytes)
            }
            model::Node::BlockDirectory => Ok(Some(Arc::new(file::SnapshotDirectory::names(
                std::iter::empty::<Vec<u8>>(),
                leaf.metadata(0, context),
            )))),
            model::Node::TtyDirectory => Ok(Some(Arc::new(file::SnapshotDirectory::names(
                [b"drivers".to_vec(), b"ldiscs".to_vec()],
                leaf.metadata(0, context),
            )))),
            model::Node::TtyDrivers => self.snapshot_file(context, 0, leaf, TTY_DRIVERS.to_vec()),
            model::Node::TtyDisciplines => self.snapshot_file(context, 0, leaf, Vec::new()),
            model::Node::BootIdentity => self.snapshot_file(
                context,
                0,
                leaf,
                Identity::new(self.source.boot_identity()?).into_bytes(),
            ),
            model::Node::RandomIdentity => self.snapshot_file(
                context,
                0,
                leaf,
                Identity::new(self.source.random_identity()?).into_bytes(),
            ),
            model::Node::EntropyAvailable | model::Node::RandomPoolSize => {
                self.snapshot_file(context, 0, leaf, RANDOM_POOL.to_vec())
            }
            model::Node::Sysctl(bytes) => self.snapshot_file(context, 0, leaf, bytes.to_vec()),
            model::Node::MemInfo => self.snapshot_file(context, 0, leaf, self.source.system()?.meminfo()),
            model::Node::Devices => self.snapshot_file(context, 0, leaf, DEVICES.to_vec()),
            model::Node::Uptime => self.snapshot_file(context, 0, leaf, self.source.system()?.uptime()),
            model::Node::LoadAverage => {
                let total = self.source.processes()?.len().max(1);
                self.snapshot_file(
                    context,
                    0,
                    leaf,
                    format!("0.00 0.00 0.00 1/{total} {current}\n").into_bytes(),
                )
            }
            model::Node::KernelCommandLine => self.snapshot_file(context, 0, leaf, KERNEL_COMMAND_LINE.to_vec()),
            model::Node::Filesystems => self.snapshot_file(context, 0, leaf, FILESYSTEMS.to_vec()),
            model::Node::Version => self.snapshot_file(context, 0, leaf, self.source.cpu()?.version()),
            model::Node::Hostname | model::Node::Domainname => {
                let uts = self.source.uts(self.source.resolve_process(current)?)?;
                Ok(Some(Arc::new(file::UtsFile::new(
                    Arc::clone(&self.source),
                    uts.namespace,
                    leaf == model::Node::Domainname,
                    leaf.metadata(current, context),
                ))))
            }
        }
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn snapshot_file(
        &self,
        context: metadata::Context,
        process: u32,
        node: model::Node,
        bytes: Vec<u8>,
    ) -> Result<Option<Arc<dyn OpenFileDescription>>, Error> {
        let metadata = node.metadata(process, context);
        Ok(Some(Arc::new(file::SnapshotFile::new(bytes, metadata))))
    }

    fn cpu_file(
        &self,
        context: metadata::Context,
        number: usize,
        node: model::Node,
        bytes: Vec<u8>,
    ) -> Result<Option<Arc<dyn OpenFileDescription>>, Error> {
        if number >= self.source.cpu()?.online() {
            return Err(Error::NotFound);
        }
        self.snapshot_file(context, 0, node, bytes)
    }
}
