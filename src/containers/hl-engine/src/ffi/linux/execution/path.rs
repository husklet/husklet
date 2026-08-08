use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Mutex};

use super::{descriptor as descriptor_table, ports, watch};

use hl_linux::{OpenAbiPlan, PathOperand};
use hl_runtime::{
    DirectoryBaseLease, GuestPath, GuestPathBytes, PreparedPathOpen, ResolvedPathLease, RuntimePathError,
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
#[path = "path/host.rs"]
mod host;
#[path = "path/lease.rs"]
mod lease;
#[path = "path/mapping.rs"]
mod mapping;
pub(in crate::ffi::linux::execution) use mapping::MappingPaths;
#[path = "path/materialization.rs"]
mod materialization;
#[path = "path/metadata.rs"]
mod metadata;
#[cfg(test)]
#[path = "path/metadata_test.rs"]
mod metadata_tests;
mod mutation;
#[path = "path/namespace.rs"]
mod namespace;
#[path = "path/native.rs"]
mod native;
#[path = "path/open.rs"]
mod open;
#[cfg(test)]
#[path = "path/overlay_bench.rs"]
mod overlay_bench;
#[path = "path/overlay_entries.rs"]
mod overlay_entries;
#[path = "path/overlay_lease.rs"]
mod overlay_lease;
#[path = "path/overlay_project.rs"]
#[cfg(test)]
mod overlay_project;
#[path = "path/overlay_publish.rs"]
mod overlay_publish;
#[path = "path/overlay_xattr.rs"]
mod overlay_xattr;
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
    locks: Option<Arc<hl_runtime::AdvisoryLockCoordinator>>,
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

struct InitialTerminalWindowNotification {
    tasks: Arc<hl_task::TaskRegistry>,
    session: hl_task::SessionId,
    terminal: Arc<hl_runtime::TerminalDescription>,
}

impl crate::composition::TerminalWindowNotification for InitialTerminalWindowNotification {
    fn changed(&self) -> Result<(), crate::composition::CompositionError> {
        if let Some(foreground) = self.terminal.pair().foreground()
            && let Some(slot) = foreground.number.checked_sub(1)
            && let Some(group) = hl_task::ProcessGroupId::from_wire(slot, foreground.generation)
        {
            let _ = self.tasks.terminal_window_changed(self.session.number(), group);
        }
        Ok(())
    }
}

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

    /// Lets copy-up report the lower and upper identities it joined, which is the
    /// only place the two are both known.
    pub(super) fn with_advisory_locks(mut self, locks: Arc<hl_runtime::AdvisoryLockCoordinator>) -> Self {
        self.locks = Some(locks);
        self
    }

    pub(super) fn new(root: &[u8], watches: Arc<watch::Hub>) -> Result<Self, RuntimePathError> {
        Ok(Self::from_source(source::Source::ordinary(root)?, watches))
    }

    pub(super) fn layered(
        upper: &[u8],
        lowers: &[Vec<u8>],
        watches: Arc<watch::Hub>,
    ) -> Result<Self, RuntimePathError> {
        Ok(Self::from_source(source::Source::layered(upper, lowers)?, watches))
    }

    fn from_source(source: source::Source, watches: Arc<watch::Hub>) -> Self {
        Self {
            source,
            paths: Arc::new(Mutex::new(BTreeMap::new())),
            synthetic_paths: Arc::new(Mutex::new(BTreeMap::new())),
            projected: projected::Registry::default(),
            writes: Arc::new(Mutex::new(BTreeMap::new())),
            ownership: Arc::new(metadata::Registry::default()),
            locks: None,
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
        }
    }

    pub(super) fn projected(root: &[u8], watches: Arc<watch::Hub>) -> Result<Self, RuntimePathError> {
        Ok(Self::from_source(source::Source::projected(root)?, watches))
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

    /// Seeds the ownership table with what the image layers declared, so a file the guest never
    /// touched still reports the uid and gid its layer shipped rather than the engine's own.
    pub(super) fn with_file_owners(self, records: &[u8], roots: Vec<std::path::PathBuf>) -> Self {
        if !records.is_empty() {
            self.ownership.declare(metadata::Declared::parse(records, roots));
        }
        self
    }

    pub(super) fn set_executable(&self, host: &[u8]) -> Result<(), RuntimePathError> {
        let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(host));
        let resolved = path.canonicalize().map_err(HostError::map)?;
        let guest = self.guest_path(&resolved)?;
        *self
            .executable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = guest.as_str().as_bytes().to_vec();
        Ok(())
    }

    pub(super) fn set_projected_executable(&self, guest: &[u8]) -> Result<(), RuntimePathError> {
        if !guest.starts_with(b"/") || guest.contains(&0) || guest.len() > 4096 {
            return Err(RuntimePathError::Invalid);
        }
        *self
            .executable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = guest.to_vec();
        Ok(())
    }

    pub(super) fn auxiliary_slot(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.auxiliary)
    }

    pub(super) fn set_auxiliary(&self, bytes: Vec<u8>) {
        *self.auxiliary.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = bytes;
    }

    pub(super) fn terminal_bindings(&self) -> Arc<hl_runtime::TerminalBindings> {
        Arc::clone(&self.terminal_bindings)
    }

    pub(super) fn initial_terminal(
        &self,
        table: Arc<hl_descriptor::DescriptorTable>,
        tasks: &Arc<hl_task::TaskRegistry>,
        process: hl_task::ProcessId,
        thread: hl_task::ThreadId,
        terminal: &Arc<crate::composition::Terminal>,
    ) -> Result<descriptor_table::Set, crate::engine::EngineError> {
        let pair = self
            .terminals
            .allocate()
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        pair.set_window(terminal.initial())
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        let session = tasks
            .session_id(process)
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        let foreground = tasks
            .process_group_id(process)
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        self.terminals
            .acquire(session.number(), pair.id())
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        tasks
            .attach_terminal(process, session)
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        tasks
            .set_foreground_group(process, foreground)
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        let (_, foreground_generation) = foreground.wire_parts();
        pair.set_foreground(hl_runtime::TerminalForegroundGroup {
            number: foreground.number(),
            generation: foreground_generation,
        })
        .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        let master = Arc::new(hl_runtime::TerminalDescription::new(
            Arc::clone(&pair),
            hl_runtime::TerminalEndpoint::Master,
            Arc::downgrade(&self.terminals),
            Arc::clone(&self.terminal_signals),
        ));
        let slave = Arc::new(hl_runtime::TerminalDescription::new(
            pair,
            hl_runtime::TerminalEndpoint::Slave,
            Arc::downgrade(&self.terminals),
            Arc::clone(&self.terminal_signals),
        ));
        let descriptors = descriptor_table::Set::with_terminal(table, slave, &self.terminal_bindings)
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        let (process, process_generation) = process.wire_parts();
        let (thread, thread_generation) = thread.wire_parts();
        let window_notification = Arc::new(InitialTerminalWindowNotification {
            tasks: Arc::clone(tasks),
            session,
            terminal: Arc::clone(&master),
        });
        terminal
            .attach(
                master,
                hl_descriptor::OperationActor {
                    process,
                    process_generation,
                    thread,
                    thread_generation,
                },
                window_notification,
            )
            .map_err(|_| crate::engine::EngineError::LaunchFailed)?;
        Ok(descriptors)
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (metadata.device, metadata.inode),
                SyntheticProvenance { guest, filesystem },
            );
        Ok(())
    }

    /// Routes through the projected overlay when one is configured.
    fn resolve_projected(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        if let Some(context) = self.source.projected_context() {
            return projected::Node::resolve(context, base, operand, &self.projected);
        }
        self.resolve_node(base, operand)
    }
}

#[cfg(test)]
mod terminal_window_test {
    use super::*;
    use crate::composition::TerminalWindowNotification as _;

    struct NoopSignal;

    impl hl_runtime::TerminalSignalSink for NoopSignal {
        fn publish(
            &self,
            _: Option<hl_descriptor::OperationActor>,
            _: hl_runtime::TerminalId,
            _: Option<hl_runtime::TerminalForegroundGroup>,
            _: hl_runtime::TerminalSignal,
        ) {
        }
    }

    #[test]
    fn external_resize_follows_current_generation_qualified_foreground() {
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        let (leader, leader_thread) = tasks
            .create_init(
                hl_task::ProcessCredentials::new(0, 0, &[], 8).unwrap(),
                hl_task::ProcessLimits::default(),
            )
            .unwrap();
        let session = tasks.session_id(leader).unwrap();
        let old = tasks.begin_fork_process(leader_thread).unwrap();
        let (old_process, old_thread) = (old.process(), old.thread());
        tasks.commit_fork_process(old).unwrap();
        let old_group = tasks.set_process_group(leader, old_process, None).unwrap();
        let new = tasks.begin_fork_process(leader_thread).unwrap();
        let (new_process, new_thread) = (new.process(), new.thread());
        tasks.commit_fork_process(new).unwrap();
        let new_group = tasks.set_process_group(leader, new_process, None).unwrap();
        for process in [old_process, new_process] {
            tasks
                .set_action(
                    process,
                    hl_task::SignalNumber::new(28).unwrap(),
                    hl_task::SignalAction {
                        disposition: hl_task::SignalDisposition::Handler(0x4000),
                        ..hl_task::SignalAction::DEFAULT
                    },
                )
                .unwrap();
        }
        tasks.set_foreground_group(leader, old_group).unwrap();

        let catalog = Arc::new(hl_runtime::TerminalCatalog::default());
        let pair = catalog.allocate().unwrap();
        let (_, old_generation) = old_group.wire_parts();
        pair.set_foreground(hl_runtime::TerminalForegroundGroup {
            number: old_group.number(),
            generation: old_generation,
        })
        .unwrap();
        let terminal = Arc::new(hl_runtime::TerminalDescription::new(
            Arc::clone(&pair),
            hl_runtime::TerminalEndpoint::Master,
            Arc::downgrade(&catalog),
            Arc::new(NoopSignal),
        ));
        let notification = InitialTerminalWindowNotification {
            tasks: Arc::clone(&tasks),
            session,
            terminal,
        };

        tasks.set_foreground_group(leader, new_group).unwrap();
        let (_, new_generation) = new_group.wire_parts();
        pair.set_foreground(hl_runtime::TerminalForegroundGroup {
            number: new_group.number(),
            generation: new_generation,
        })
        .unwrap();
        notification.changed().unwrap();
        assert_eq!(tasks.pending_signal_mask(old_thread).unwrap().bits(), 0);
        assert_ne!(tasks.pending_signal_mask(new_thread).unwrap().bits() & (1 << 27), 0);

        pair.set_foreground(hl_runtime::TerminalForegroundGroup {
            number: new_group.number(),
            generation: new_generation.saturating_add(1),
        })
        .unwrap();
        notification.changed().unwrap();
        assert_eq!(tasks.pending_signal_mask(old_thread).unwrap().bits(), 0);
    }
}
