use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex};

use super::{ports, watch};

use hl_linux::{OpenAbiPlan, PathOperand};
use hl_runtime::{
    AccessIdentity, DirectoryBaseLease, GuestPath, GuestPathBytes, OpenIntent, PreparedPathMutation, PreparedPathOpen,
    ResolveConstraints, ResolveError, ResolveRequest, ResolvedPathLease, Resolver, RuntimePathError, RuntimePathHost,
};

#[path = "path/attribute.rs"]
mod attribute;
#[path = "path/auxiliary.rs"]
mod auxiliary;
#[path = "path/device.rs"]
mod device;
#[path = "path/directory.rs"]
mod directory;
#[cfg(test)]
#[path = "path/directory_test.rs"]
mod directory_tests;
#[path = "path/entries.rs"]
mod entries;
#[path = "path/error.rs"]
mod error;
#[path = "path/executable.rs"]
mod executable;
mod fifo;
#[path = "path/file.rs"]
mod file;
#[path = "path/filesystem.rs"]
mod filesystem;
#[path = "path/lease.rs"]
mod lease;
#[path = "path/mapping.rs"]
mod mapping;
pub(in crate::ffi::linux::execution) use mapping::MappingPaths;
#[path = "path/materialization.rs"]
mod materialization;
#[path = "path/metadata.rs"]
mod metadata;
mod mutation;
#[path = "path/namespace.rs"]
mod namespace;
#[path = "path/native.rs"]
mod native;
#[path = "path/open.rs"]
mod open;
#[path = "path/pin.rs"]
mod pin;
mod proc;
#[cfg(test)]
#[path = "path/proc_test.rs"]
mod proc_test;
mod projected;
#[path = "path/registry.rs"]
mod registry;
#[path = "path/resolution.rs"]
mod resolution;
#[path = "path/route.rs"]
mod route;
#[path = "path/source.rs"]
mod source;
#[cfg(test)]
#[path = "path/source_test.rs"]
mod source_tests;
#[path = "path/splice.rs"]
mod splice;
#[path = "path/tmpfs.rs"]
mod tmpfs;
#[path = "path/transfer.rs"]
mod transfer;
mod unix_socket;
use error::HostError;
pub(in crate::ffi::linux::execution) use executable::ExecTarget;
pub(in crate::ffi::linux::execution) use file::NativeFile;
pub(super) use unix_socket::UnixSocketPaths;
#[derive(Clone)]
pub(super) struct NativePath {
    source: source::Source,
    paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
    synthetic_paths: Arc<Mutex<BTreeMap<(u64, u64), SyntheticProvenance>>>,
    projected: projected::Registry,
    writes: Arc<Mutex<BTreeMap<(u64, u64), usize>>>,
    ownership: Arc<metadata::Registry>,
    watches: Arc<watch::Hub>,
    executable: Arc<Mutex<Vec<u8>>>,
    auxiliary: Arc<Mutex<Vec<u8>>>,
    namespace_root: Arc<Vec<u8>>,
    procfs: Option<Arc<hl_runtime::Procfs>>,
    tasks: Option<Arc<hl_task::TaskRegistry>>,
    process: Option<hl_task::ProcessId>,
    thread: Option<hl_task::ThreadId>,
    namespace_handles: Option<Arc<hl_runtime::NamespaceHandleRegistry>>,
    terminals: Arc<hl_runtime::TerminalCatalog>,
    terminal_bindings: Arc<hl_runtime::TerminalBindings>,
    terminal_signals: Arc<dyn hl_runtime::TerminalSignalSink>,
    entropy: Arc<dyn ports::random::EntropySource>,
    transfers: Arc<transfer::FileTransferRegistry>,
    fifos: Arc<fifo::Registry>,
    system: Option<Arc<hl_runtime::SystemAuthority>>,
    cpu_model: hl_runtime::ProcfsCpuModel,
}

#[derive(Clone)]
struct SyntheticProvenance {
    guest: GuestPath,
    filesystem: hl_runtime::FilesystemStats,
}

pub(super) use transfer::{FileIntent, FileOperation, FileTransferRegistry};

impl NativePath {
    pub(in crate::ffi::linux::execution) fn exec_image(&self, executable: Vec<u8>, auxiliary: Vec<u8>) -> Arc<Self> {
        let mut image = self.clone();
        image.executable = Arc::new(Mutex::new(executable));
        image.auxiliary = Arc::new(Mutex::new(auxiliary));
        Arc::new(image)
    }

    pub(super) fn terminal_catalog(&self) -> Arc<hl_runtime::TerminalCatalog> {
        Arc::clone(&self.terminals)
    }

    fn terminal_session(&self) -> Option<(hl_task::SessionId, bool, bool)> {
        let process = self.process?;
        let tasks = self.tasks.as_ref()?;
        let session = tasks.session_id(process).ok()?;
        let attached = tasks.terminal_session(process).ok()?.is_some();
        let snapshot = tasks.snapshot();
        let leader = snapshot.sessions.iter().find(|entry| entry.id == session)?.leader == process;
        Some((session, leader, attached))
    }

    fn synthetic_plan(base: &DirectoryBaseLease, plan: &OpenAbiPlan) -> Result<Option<OpenAbiPlan>, RuntimePathError> {
        if plan.operand.path.is_absolute() {
            return Ok(None);
        }
        let operand = std::str::from_utf8(plan.operand.path.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let joined = format!("{}/{}", base.path().as_str().trim_end_matches('/'), operand);
        let normalized = GuestPath::new(&joined).map_err(|_| RuntimePathError::Invalid)?;
        let mut absolute = plan.clone();
        absolute.operand.path =
            GuestPathBytes::new(normalized.as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        Ok(Some(absolute))
    }

    pub(super) fn with_cpu_model(mut self, model: hl_runtime::ProcfsCpuModel) -> Self {
        self.cpu_model = model;
        self
    }

    pub(super) fn new(root: &[u8], watches: Arc<watch::Hub>) -> Result<Self, RuntimePathError> {
        Ok(Self {
            source: source::Source::ordinary(root)?,
            paths: Arc::new(Mutex::new(BTreeMap::new())),
            synthetic_paths: Arc::new(Mutex::new(BTreeMap::new())),
            projected: projected::Registry::default(),
            writes: Arc::new(Mutex::new(BTreeMap::new())),
            ownership: Arc::new(metadata::Registry::default()),
            watches,
            executable: Arc::new(Mutex::new(Vec::new())),
            auxiliary: Arc::new(Mutex::new(Vec::new())),
            namespace_root: Arc::new(b"/".to_vec()),
            procfs: None,
            tasks: None,
            process: None,
            thread: None,
            namespace_handles: None,
            terminals: Arc::new(hl_runtime::TerminalCatalog::default()),
            terminal_bindings: Arc::new(hl_runtime::TerminalBindings::default()),
            terminal_signals: Arc::new(device::DetachedSignals),
            entropy: Arc::new(super::image_data::Entropy),
            transfers: Arc::new(transfer::FileTransferRegistry::default()),
            fifos: Arc::new(fifo::Registry::default()),
            system: None,
            cpu_model: hl_runtime::ProcfsCpuModel::Aarch64 {
                hardware: 0,
                hardware_second: 0,
            },
        })
    }

    pub(super) fn projected(root: &[u8], watches: Arc<watch::Hub>) -> Result<Self, RuntimePathError> {
        Ok(Self {
            source: source::Source::projected(root)?,
            paths: Arc::new(Mutex::new(BTreeMap::new())),
            synthetic_paths: Arc::new(Mutex::new(BTreeMap::new())),
            projected: projected::Registry::default(),
            writes: Arc::new(Mutex::new(BTreeMap::new())),
            ownership: Arc::new(metadata::Registry::default()),
            watches,
            executable: Arc::new(Mutex::new(Vec::new())),
            auxiliary: Arc::new(Mutex::new(Vec::new())),
            namespace_root: Arc::new(b"/".to_vec()),
            procfs: None,
            tasks: None,
            process: None,
            thread: None,
            namespace_handles: None,
            terminals: Arc::new(hl_runtime::TerminalCatalog::default()),
            terminal_bindings: Arc::new(hl_runtime::TerminalBindings::default()),
            terminal_signals: Arc::new(device::DetachedSignals),
            entropy: Arc::new(super::image_data::Entropy),
            transfers: Arc::new(transfer::FileTransferRegistry::default()),
            fifos: Arc::new(fifo::Registry::default()),
            system: None,
            cpu_model: hl_runtime::ProcfsCpuModel::Aarch64 {
                hardware: 0,
                hardware_second: 0,
            },
        })
    }

    pub(super) fn ordinary(&self) -> Result<&source::OrdinaryContext, RuntimePathError> {
        self.source.native()
    }

    pub(super) fn with_projection(
        mut self,
        tree: Arc<Mutex<crate::native::AuthorityWorker>>,
    ) -> Result<Self, RuntimePathError> {
        self.source = self.source.with_tree(tree)?;
        Ok(self)
    }

    pub(super) fn with_process(
        mut self,
        tasks: Arc<hl_task::TaskRegistry>,
        process: hl_task::ProcessId,
        handles: Arc<hl_runtime::NamespaceHandleRegistry>,
        descriptors: Arc<hl_descriptor::DescriptorTable>,
    ) -> Self {
        self.terminal_signals = Arc::new(hl_runtime::TerminalSignals::new(Arc::clone(&tasks)));
        let source = proc::source(
            Arc::clone(&tasks),
            process,
            descriptors,
            Arc::clone(&self.paths),
            self.projected.clone(),
            self.system.as_ref(),
            self.namespace_root.as_slice(),
            Arc::new(hl_runtime::WorkingDirectory::root()),
            Arc::new(hl_runtime::FsContext::default()),
            self.source.mount_port(),
            None,
            None,
            None,
            self.cpu_model.clone(),
            None,
            hl_linux::SeccompBaseline::Container,
        );
        self.procfs = Some(Arc::new(hl_runtime::Procfs::new(Arc::new(source))));
        self.tasks = Some(tasks);
        self.process = Some(process);
        self.thread = None;
        self.namespace_handles = Some(handles);
        self
    }

    pub(super) fn with_read_only(self, enabled: bool) -> Self {
        if let Ok(context) = self.source.native() {
            context.set_root_policy(enabled);
        }
        self
    }

    pub(super) fn with_transfers(mut self, transfers: Arc<transfer::FileTransferRegistry>) -> Self {
        self.transfers = transfers;
        self
    }

    pub(super) fn set_executable(&self, host: &[u8]) -> Result<(), RuntimePathError> {
        let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(host));
        let resolved = path.canonicalize().map_err(HostError::map)?;
        let guest = self.guest_path(&resolved)?;
        *self.executable.lock().unwrap_or_else(|error| error.into_inner()) = guest.as_str().as_bytes().to_vec();
        Ok(())
    }

    pub(super) fn set_projected_executable(&self, guest: &[u8]) -> Result<(), RuntimePathError> {
        if !guest.starts_with(b"/") || guest.contains(&0) || guest.len() > 4096 {
            return Err(RuntimePathError::Invalid);
        }
        *self.executable.lock().unwrap_or_else(|error| error.into_inner()) = guest.to_vec();
        Ok(())
    }

    pub(super) fn auxiliary_slot(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.auxiliary)
    }

    pub(super) fn set_auxiliary(&self, bytes: Vec<u8>) {
        *self.auxiliary.lock().unwrap_or_else(|error| error.into_inner()) = bytes;
    }

    pub(super) fn terminal_bindings(&self) -> Arc<hl_runtime::TerminalBindings> {
        Arc::clone(&self.terminal_bindings)
    }

    fn publish_synthetic(&self, plan: &OpenAbiPlan, opened: &dyn PreparedPathOpen) -> Result<(), RuntimePathError> {
        let Ok(metadata) = opened.object().metadata() else {
            return Ok(());
        };
        let path = std::str::from_utf8(plan.operand.path.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let guest = GuestPath::new(path).map_err(|_| RuntimePathError::Invalid)?;
        let filesystem = filesystem::HostFilesystem::synthetic(&guest);
        self.synthetic_paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                (metadata.device, metadata.inode),
                SyntheticProvenance { guest, filesystem },
            );
        Ok(())
    }
}

impl RuntimePathHost for NativePath {
    fn root_base(&self) -> Result<DirectoryBaseLease, RuntimePathError> {
        Ok(DirectoryBaseLease::root(
            GuestPath::new("/").map_err(|_| RuntimePathError::Invalid)?,
        ))
    }

    fn working_base(&self, path: GuestPath) -> Result<DirectoryBaseLease, RuntimePathError> {
        if self.source.is_projected() {
            return if path.as_str() == "/" {
                Ok(DirectoryBaseLease::root(path))
            } else {
                Err(RuntimePathError::Unsupported)
            };
        }
        let ordinary = self.ordinary()?;
        let relative = path.as_str().strip_prefix('/').ok_or(RuntimePathError::Invalid)?;
        let resolved = ordinary.root().join(relative).canonicalize().map_err(HostError::map)?;
        if !resolved.starts_with(ordinary.root()) {
            return Err(RuntimePathError::Access);
        }
        if !resolved.is_dir() {
            return Err(RuntimePathError::NotDirectory);
        }
        Ok(DirectoryBaseLease::root(path))
    }

    fn descriptor_base(&self, lease: hl_descriptor::OperationLease) -> Result<DirectoryBaseLease, RuntimePathError> {
        let metadata = lease.metadata().map_err(|_| RuntimePathError::BadDescriptor)?;
        if metadata.kind != 4 {
            return Err(RuntimePathError::NotDirectory);
        }
        let key = (metadata.device, metadata.inode);
        if let Some(path) = self
            .synthetic_paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .map(|provenance| provenance.guest.clone())
        {
            return Ok(DirectoryBaseLease::descriptor(lease, path));
        }
        if self.source.is_projected() {
            let file = self.projected.get(&key).ok_or(RuntimePathError::NotFound)?;
            return Ok(DirectoryBaseLease::descriptor(lease, file.guest()?));
        }
        let ordinary = self.ordinary()?;
        let opened = self
            .paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .cloned()
            .ok_or(RuntimePathError::NotFound)?;
        let host = ordinary.root().join(opened.guest.as_str().trim_start_matches('/'));
        let current = std::fs::metadata(host).map_err(HostError::map)?;
        if (current.dev(), current.ino()) != key {
            return Err(RuntimePathError::NotFound);
        }
        Ok(DirectoryBaseLease::descriptor(lease, opened.guest))
    }

    fn directory_path(&self, base: &DirectoryBaseLease, operand: &PathOperand) -> Result<GuestPath, RuntimePathError> {
        if operand.path.is_absolute() && !base.confines_root() {
            let path = std::str::from_utf8(operand.path.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
            let path = GuestPath::new(path).map_err(|_| RuntimePathError::Invalid)?;
            if path.as_str() == "/" {
                return Ok(path);
            }
        }
        if let Some(context) = self.source.projected_context() {
            let node = projected::Node::resolve(context, base, operand, &self.projected)?;
            if node.metadata()?.kind != hl_runtime::FileKind::Directory {
                return Err(RuntimePathError::NotDirectory);
            }
            return projected::Path::join(context.root(), base, operand.path.as_bytes()).and_then(|path| {
                GuestPath::new(std::str::from_utf8(&path).map_err(|_| RuntimePathError::Invalid)?)
                    .map_err(|_| RuntimePathError::Invalid)
            });
        }
        let path = self.resolve_path(base, operand)?;
        if !path.is_dir() {
            return Err(RuntimePathError::NotDirectory);
        }
        self.guest_path(&path)
    }

    fn descriptor_node(
        &self,
        lease: hl_descriptor::OperationLease,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        let metadata = lease.metadata().map_err(|_| RuntimePathError::BadDescriptor)?;
        let identity = (metadata.device, metadata.inode);
        let filesystem = self
            .paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&identity)
            .map(|opened| opened.filesystem)
            .or_else(|| {
                self.synthetic_paths
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&identity)
                    .map(|provenance| provenance.filesystem)
            });
        Ok(Box::new(metadata::DescriptorNode::new(
            lease,
            filesystem,
            Arc::clone(&self.ownership),
        )))
    }

    fn filesystem(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<hl_runtime::FilesystemStats, RuntimePathError> {
        if operand.path.is_absolute() && !base.confines_root() {
            let path = std::str::from_utf8(operand.path.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
            let guest = hl_runtime::GuestPath::new(path).map_err(|_| RuntimePathError::Invalid)?;
            let ordinary = self.ordinary()?;
            filesystem::HostFilesystem::read(ordinary.root(), &guest)
        } else {
            self.resolve(base, operand)?.filesystem()
        }
    }

    fn resolve(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        if !base.confines_root()
            && let Some(node) = self.procfs_node(operand.path.as_bytes())?
        {
            if !operand.nofollow && self.procfs_namespace(operand.path.as_bytes())? {
                return Ok(Box::new(node.follow_namespace()));
            }
            if let Some(target) = node.target().filter(|_| !operand.nofollow) {
                let target = PathOperand {
                    directory: operand.directory,
                    path: GuestPathBytes::new(target).map_err(|_| RuntimePathError::Invalid)?,
                    allow_empty: false,
                    nofollow: false,
                };
                if let Some(context) = self.source.projected_context() {
                    return projected::Node::resolve(context, base, &target, &self.projected);
                }
                return self.resolve_node(base, &target);
            } else {
                return Ok(Box::new(node));
            }
        }
        if !base.confines_root() && operand.path.as_bytes() == b"/proc/self/exe" {
            return self.resolve_node(base, operand);
        }
        if let Some(context) = self.source.projected_context() {
            return projected::Node::resolve(context, base, operand, &self.projected);
        }
        self.resolve_node(base, operand)
    }

    fn resolve_executable(
        &self,
        base: &DirectoryBaseLease,
        path: &hl_runtime::ExecutablePath,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        self.open_executable(base, path)
    }

    fn prepare_open(
        &self,
        base: &DirectoryBaseLease,
        plan: &OpenAbiPlan,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        let redirected = if base.confines_root() {
            None
        } else {
            self.procfs_plan(plan)?
        };
        let plan = redirected.as_ref().unwrap_or(plan);
        resolution::Policy::from(plan.resolve).admit()?;
        if !base.confines_root()
            && let Some(opened) = device::TerminalOpen::prepare(
                plan.operand.path.as_bytes(),
                plan.intent,
                plan.nonblocking,
                &self.terminals,
                &self.terminal_bindings,
                &self.terminal_signals,
                self.terminal_session(),
                self.tasks
                    .as_ref()
                    .zip(self.process)
                    .map(|(tasks, process)| (Arc::clone(tasks), process)),
                plan.no_controlling_terminal,
            )?
        {
            self.publish_synthetic(plan, opened.as_ref())?;
            return Ok(opened);
        }
        let synthetic_plan = Self::synthetic_plan(base, plan)?;
        let synthetic_plan = synthetic_plan.as_ref().unwrap_or(plan);
        if !base.confines_root()
            && let Some(opened) = self.synthetic_open(synthetic_plan)?
        {
            self.publish_synthetic(synthetic_plan, opened.as_ref())?;
            return Ok(opened);
        }
        if let Some(context) = self.source.projected_context() {
            return projected::Open::prepare(context, base, plan, self.projected.clone());
        }
        if !base.confines_root()
            && let Some(opened) = self.builtin(plan)?
        {
            self.publish_synthetic(plan, opened.as_ref())?;
            return Ok(opened);
        }
        let ordinary = self.ordinary()?;
        let name_binding = ordinary.name_binding(base.path(), plan.operand.path.as_bytes())?;
        if name_binding.is_some()
            && plan.intent.bits() & (OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::TRUNCATE | OpenIntent::APPEND)
                != 0
            && plan.intent.bits() & OpenIntent::PATH_ONLY == 0
        {
            return Err(RuntimePathError::ReadOnly);
        }
        if let Some(binding) = name_binding {
            let parent: std::os::fd::OwnedFd = binding.parent.try_clone().map_err(HostError::map)?.into();
            let file = NativeFile::new(
                Arc::clone(&self.watches),
                binding.host.clone(),
                Arc::clone(&self.writes),
                Arc::clone(&self.ownership),
                None,
            );
            return Ok(Box::new(open::PendingOpen::projected(
                file,
                binding.host,
                binding.guest,
                plan.intent,
                plan.mode,
                Arc::clone(&self.paths),
                Arc::clone(&self.terminals),
                parent,
                binding.leaf,
                Arc::clone(&self.transfers),
            )));
        }
        let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
        let base_path = GuestPathBytes::new(base.path().as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let request = ResolveRequest {
            path: &plan.operand.path,
            base: &base_path,
            nofollow_final: plan.operand.nofollow,
            no_symlinks: plan.resolve.no_symlinks,
            allow_missing_final: plan.intent.bits() & OpenIntent::CREATE != 0,
        };
        let constraints = ResolveConstraints {
            no_cross_device: plan.resolve.no_cross_device,
            no_magic_links: plan.resolve.no_magic_links,
            beneath: plan.resolve.beneath,
            in_root: plan.resolve.in_root || base.confines_root(),
        };
        let resolved = resolver
            .resolve_with(request, constraints)
            .map_err(resolution::Policy::runtime_error)?;
        let parent = resolved
            .duplicate_parent()
            .map_err(|error| resolution::Policy::runtime_error(ResolveError::Host(error)))?;
        let name = CString::new(resolved.final_name().map_or(b".".as_slice(), |name| name.as_bytes()))
            .map_err(|_| RuntimePathError::Invalid)?;
        let path = pin::Host::path(&parent, &name)?;
        let guest =
            GuestPathBytes::new(self.guest_path(&path)?.as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        resolution::Policy::writable(
            ordinary.mounts(),
            ordinary.read_only(),
            ordinary.root_read_only(),
            &guest,
            plan.intent,
        )?;
        let file = NativeFile::new(
            Arc::clone(&self.watches),
            path.clone(),
            Arc::clone(&self.writes),
            Arc::clone(&self.ownership),
            ordinary.shm_budget(&path),
        );
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the pinned parent and terminated name remain live and status is writable.
        let exists = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        if exists {
            // SAFETY: successful fstatat initialized status.
            let status = unsafe { status.assume_init() };
            if status.st_mode & libc::S_IFMT == libc::S_IFIFO {
                let key = hl_ipc::NamedFifoKey {
                    device: status.st_dev as u64,
                    inode: status.st_ino as u64,
                };
                return self.fifos.prepare(key, plan);
            }
        }
        Ok(Box::new(open::PendingOpen::new(
            file,
            path,
            plan.intent,
            plan.mode,
            ordinary.root().to_path_buf(),
            Arc::clone(&self.paths),
            Arc::clone(&self.terminals),
            parent,
            name,
            Arc::clone(&self.transfers),
        )))
    }

    fn open_may_block(&self, base: &DirectoryBaseLease, plan: &OpenAbiPlan) -> Result<bool, RuntimePathError> {
        let bits = plan.intent.bits();
        if plan.nonblocking || bits & OpenIntent::READ != 0 && bits & OpenIntent::WRITE != 0 {
            return Ok(false);
        }
        // Projected opens cross an authority/provider boundary. The provider
        // is allowed to block independently of the resolved node kind.
        if self.source.is_projected() {
            return Ok(true);
        }
        Ok(self.resolve(base, &plan.operand)?.metadata()?.kind == hl_runtime::FileKind::Fifo)
    }

    fn prepare_mutation(
        &self,
        bases: &[DirectoryBaseLease],
        plan: &hl_linux::FsMutationPlan,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        mutation::prepare(self, bases, plan, identity)
    }

    fn prepare_inode_link(
        &self,
        source: hl_descriptor::OperationLease,
        target_base: &DirectoryBaseLease,
        target: &PathOperand,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        mutation::prepare_inode_link(self, source, target_base, target, identity)
    }

    fn prepare_descriptor_chmod(
        &self,
        source: hl_descriptor::OperationLease,
        mode: u32,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        attribute::prepare_chmod(source, mode, identity)
    }

    fn prepare_descriptor_chown(
        &self,
        source: hl_descriptor::OperationLease,
        user: Option<u32>,
        group: Option<u32>,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        attribute::prepare_chown(self, source, user, group, identity)
    }

    fn prepare_descriptor_times(
        &self,
        source: hl_descriptor::OperationLease,
        times: [hl_linux::TimestampChange; 2],
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        attribute::prepare_times(source, times, identity)
    }

    fn access_identity(&self) -> Result<AccessIdentity, RuntimePathError> {
        mutation::Identity::project(self)
    }

    fn access_identity_for(&self, effective: bool) -> Result<AccessIdentity, RuntimePathError> {
        mutation::Identity::access(self, effective)
    }
}
