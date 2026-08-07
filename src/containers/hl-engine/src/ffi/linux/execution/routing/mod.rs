//! Process-scoped Linux runtime routing and its owned composition adapters.
use super::process_memory::ProcessMemory;
use super::{MappingHostAdapter, RuntimeLaunchPlan, VirtualMemory};
use super::{descriptor as descriptor_table, fork, itimer, network, path, ports, readiness, task, threads, watch};
use hl_isa::GuestAddress;
use hl_memory::MappingCoordinator;
use hl_runtime::{
    BrkRegion, BrkSnapshot, OperationRegistry, RuntimeAssembly, RuntimeMemorySyscalls, RuntimePathHost,
    RuntimeSyscallRouter,
};
use hl_task::{Limit, ProcessCredentials, ProcessLimits, Resource};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

mod aio;
mod composition;
mod context;
pub(in crate::ffi::linux::execution) use composition::{host_group, host_user, launch_identity};
mod descriptor;
mod event_checkpoint;
pub(super) mod image;
mod lifecycle;
mod ptrace;
mod router;
mod signal;
use super::exit::runtime as exit_runtime;
use super::syscall::MemoryRuntime;
use crate::engine::EngineError;
pub(super) use composition::create;

pub(super) struct Route {
    pub(super) router: RuntimeSyscallRouter,
    pub(super) thread: hl_task::ThreadId,
    pub(super) process: Arc<ProcessContext>,
}

const TABLE_CAPACITY: usize = 128;

struct TableAdmission {
    active: AtomicUsize,
}

impl TableAdmission {
    fn with_root() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(1),
        })
    }

    fn reserve(self: &Arc<Self>) -> Option<Arc<TablePermit>> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < TABLE_CAPACITY).then_some(active + 1)
            })
            .ok()
            .map(|_| Arc::new(TablePermit(Arc::clone(self))))
    }
}

struct TablePermit(Arc<TableAdmission>);

impl Drop for TablePermit {
    fn drop(&mut self) {
        let previous = self.0.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 1, "the root descriptor-table admission is permanent");
    }
}

struct PrivateTable {
    table: Weak<hl_runtime::RuntimeDescriptorTable>,
    permit: Arc<TablePermit>,
}

fn fork_files(
    epoll: &hl_runtime::EpollControl,
    descriptors: &descriptor_table::Set,
    source: &hl_runtime::RuntimeDescriptorTable,
) -> (Arc<descriptor_table::Set>, Arc<hl_runtime::RuntimeDescriptorTable>) {
    let table = Arc::new(epoll.fork(source));
    let descriptors = Arc::new(descriptors.fork(table.descriptor_table()));
    (descriptors, table)
}

pub(super) struct ProcessContext {
    projected: bool,
    aio: Arc<hl_runtime::AioCatalog>,
    descriptors: Arc<descriptor_table::Set>,
    entropy: Arc<dyn ports::random::EntropySource>,
    handles: Arc<hl_runtime::ProcessHandleRegistry>,
    namespace_handles: Arc<hl_runtime::NamespaceHandleRegistry>,
    events: Arc<hl_runtime::EventCatalog>,
    epoll: Arc<hl_runtime::EpollControl>,
    epoll_table: Arc<hl_runtime::RuntimeDescriptorTable>,
    table_admission: Arc<TableAdmission>,
    table_permit: Option<Arc<TablePermit>>,
    thread_files: Mutex<BTreeMap<hl_task::ThreadId, PrivateTable>>,
    network: Arc<network::CheckpointRuntime>,
    network_enabled: bool,
    event_operations: Arc<OperationRegistry>,
    event_checkpoint: event_checkpoint::Resources,
    tasks: Arc<hl_task::TaskRegistry>,
    seccomp: Arc<hl_runtime::SeccompControl>,
    seccomp_baseline: hl_linux::SeccompBaseline,
    process: hl_task::ProcessId,
    memory: Arc<Mutex<MemoryRuntime>>,
    clock: Arc<task::ClockIdentity>,
    deadlines: Arc<readiness::deadline::Queue>,
    alarms: Arc<hl_runtime::AlarmRegistry>,
    timers: Arc<hl_runtime::TimerRegistry>,
    exec: Arc<hl_runtime::ExecSlot>,
    exec_queue: Arc<hl_runtime::ExecQueue>,
    futex: Arc<hl_runtime::SafeRuntimeFutex<MappingHostAdapter>>,
    interruptions: Arc<task::FutexInterrupt>,
    architecture: hl_linux::GuestArchitecture,
    trace: bool,
    path_host: Option<Arc<path::NativePath>>,
    watches: Option<Arc<watch::Hub>>,
    space: Arc<super::space::AddressSpace>,
    procfs_spaces: Arc<super::process_memory::ProcfsSpaces>,
    procfs_resources: Arc<super::process_resources::Catalog>,
    fork: OnceLock<Weak<fork::Runtime>>,
    threads: OnceLock<Weak<threads::ThreadSet>>,
    clone_context: OnceLock<Weak<super::clone::Contexts>>,
    exec_coordinator: OnceLock<Weak<super::exec_image::Coordinator>>,
    exec_registration: OnceLock<Arc<super::exec_image::Registration>>,
    sigreturn_pc: u64,
    working: Arc<hl_runtime::WorkingDirectory>,
    fs_context: Arc<hl_runtime::FsContext>,
    ipc_catalog: Arc<hl_runtime::IpcCatalog>,
    posix_queues: Arc<hl_runtime::MqNamespace>,
    ipc_pipes: Arc<hl_runtime::IpcPipeRegistry>,
    ipc: Arc<hl_runtime::MemoryMappings<MappingHostAdapter>>,
    locks: Arc<hl_runtime::AdvisoryLockCoordinator>,
    exit: Arc<hl_runtime::ExitRuntime>,
    ptrace: Arc<hl_runtime::PtraceCatalog>,
    system: Arc<hl_runtime::SystemAuthority>,
    vfork: Arc<OnceLock<Arc<hl_runtime::VforkParentToken>>>,
}

impl ProcessContext {
    pub(super) const fn process_id(&self) -> hl_task::ProcessId {
        self.process
    }

    pub(super) fn memfd_registry(&self) -> Result<Arc<hl_runtime::MemfdRegistry>, EngineError> {
        self.memory
            .lock()
            .map_err(|_| EngineError::Synchronization)
            .map(|memory| memory.memfd_registry())
    }

    pub(super) fn handles(&self) -> Arc<hl_runtime::ProcessHandleRegistry> {
        Arc::clone(&self.handles)
    }

    pub(super) fn observe_fork(&self) {
        self.system.observe_fork();
    }

    pub(super) fn fork_child(
        &self,
        source: hl_task::ThreadId,
        process: hl_task::ProcessId,
        space: Arc<super::space::AddressSpace>,
        ipc: Arc<hl_runtime::IpcForkChild<MappingHostAdapter>>,
    ) -> Result<Arc<Self>, EngineError> {
        let table_permit = self.table_admission.reserve().ok_or(EngineError::LaunchFailed)?;
        let thread = self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|entry| entry.id == process)
            .map(|entry| entry.leader)
            .ok_or(EngineError::LaunchFailed)?;
        self.seccomp
            .fork(source, thread)
            .map_err(|_| EngineError::LaunchFailed)?;
        let source_files = self.files(source);
        let (descriptors, epoll_table) = fork_files(&self.epoll, &self.descriptors, &source_files);
        let table = epoll_table.descriptor_table();
        let working = Arc::new(hl_runtime::WorkingDirectory::from_snapshot(self.working.snapshot()));
        let mappings = space.mappings();
        let process_memory = space.guest_memory();
        let trace_exchange = hl_runtime::TraceExchange::new(Arc::new(process_memory.clone()));
        self.ptrace.register(process, Arc::clone(&trace_exchange));
        let memory = self
            .memory
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .fork_clone(
                Arc::clone(&mappings),
                Arc::clone(&table),
                process_memory,
                u64::from(process.number()),
            )
            .map_err(|_| EngineError::LaunchFailed)?
            .with_host(Arc::new(super::super::memory_control::Control::new(
                space.arena(),
                Arc::clone(&mappings),
                Arc::new(super::memory_limit::MemoryLimit::new(Arc::clone(&self.tasks), process)),
            )));
        let interruptions = Arc::new(task::FutexInterrupt::new());
        let futex = Arc::new(
            self.futex
                .fork(Arc::clone(&mappings), interruptions.clone())
                .map_err(|_| EngineError::LaunchFailed)?,
        );
        let descriptor_image = epoll_table.image_slot();
        if !Arc::ptr_eq(&ipc.memory, &mappings) || !Arc::ptr_eq(&ipc.catalog, &self.ipc_catalog) {
            return Err(EngineError::LaunchFailed);
        }
        let vfork = Arc::new(OnceLock::new());
        let exit = exit_runtime(
            Arc::clone(&self.tasks),
            mappings,
            Arc::clone(&descriptor_image),
            Arc::clone(&self.epoll),
            futex.clone(),
            Arc::clone(&self.ipc_catalog),
            Arc::clone(&ipc.mappings),
            Arc::clone(&self.locks),
            Arc::clone(&self.clock),
            Arc::clone(&vfork),
            Arc::clone(&self.handles),
            Arc::clone(&self.ptrace),
            Arc::clone(&self.procfs_spaces),
            self.path_host.as_ref().map(|host| host.terminal_catalog()),
        );
        let child = Arc::new(Self {
            projected: self.projected,
            aio: Arc::new(hl_runtime::AioCatalog::default()),
            descriptors,
            entropy: Arc::clone(&self.entropy),
            handles: Arc::clone(&self.handles),
            namespace_handles: Arc::clone(&self.namespace_handles),
            events: Arc::clone(&self.events),
            epoll: Arc::clone(&self.epoll),
            epoll_table,
            table_admission: Arc::clone(&self.table_admission),
            table_permit: Some(table_permit),
            thread_files: Mutex::new(BTreeMap::new()),
            network: Arc::clone(&self.network),
            network_enabled: self.network_enabled,
            event_operations: Arc::clone(&self.event_operations),
            event_checkpoint: self.event_checkpoint.clone(),
            tasks: Arc::clone(&self.tasks),
            seccomp: Arc::clone(&self.seccomp),
            seccomp_baseline: self.seccomp_baseline,
            process,
            memory: Arc::new(Mutex::new(memory)),
            clock: Arc::clone(&self.clock),
            deadlines: Arc::clone(&self.deadlines),
            alarms: Arc::clone(&self.alarms),
            timers: hl_runtime::TimerRegistry::new(process, Arc::clone(&self.alarms)),
            exec: Arc::clone(&self.exec),
            exec_queue: Arc::clone(&self.exec_queue),
            futex,
            interruptions,
            architecture: self.architecture,
            trace: self.trace,
            path_host: self.path_host.clone(),
            watches: self.watches.clone(),
            space,
            procfs_spaces: Arc::clone(&self.procfs_spaces),
            procfs_resources: Arc::clone(&self.procfs_resources),
            fork: OnceLock::new(),
            threads: OnceLock::new(),
            clone_context: OnceLock::new(),
            exec_coordinator: OnceLock::new(),
            exec_registration: OnceLock::new(),
            sigreturn_pc: self.sigreturn_pc,
            working,
            fs_context: Arc::new(self.fs_context.fork_copy()),
            ipc_catalog: Arc::clone(&self.ipc_catalog),
            posix_queues: Arc::clone(&self.posix_queues),
            ipc: Arc::clone(&ipc.mappings),
            ipc_pipes: Arc::clone(&self.ipc_pipes),
            locks: Arc::clone(&self.locks),
            exit,
            ptrace: Arc::clone(&self.ptrace),
            system: Arc::clone(&self.system),
            vfork,
        });
        if let Some(parent) = self.exec_coordinator.get().and_then(Weak::upgrade) {
            let coordinator = parent.fork(Arc::clone(&child));
            child
                .install_exec(&coordinator)
                .map_err(|()| EngineError::LaunchFailed)?;
            self.exec
                .register(process, coordinator)
                .map_err(|_| EngineError::LaunchFailed)?;
            child
                .install_registration(super::exec_image::Registration::new(Arc::clone(&self.exec), process))
                .map_err(|()| EngineError::LaunchFailed)?;
        }
        Ok(child)
    }

    pub(super) fn install_vfork(&self, token: Arc<hl_runtime::VforkParentToken>) -> Result<(), EngineError> {
        self.vfork.set(token).map_err(|_| EngineError::LaunchFailed)
    }

    pub(super) fn vfork_token(&self) -> Option<Arc<hl_runtime::VforkParentToken>> {
        self.vfork.get().cloned()
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    #[test]
    fn fork_uses_one_process_descriptor_table() {
        let (epoll, source) = hl_runtime::EpollControl::new(64, 64).unwrap();
        let descriptors = super::descriptor_table::Set::with_table(
            source.descriptor_table(),
            &crate::composition::StandardStreams::new(std::io::empty(), std::io::sink(), std::io::sink()),
        )
        .unwrap();
        let (descriptors, table) = super::fork_files(&epoll, &descriptors, &source);
        assert!(Arc::ptr_eq(&descriptors.descriptor_table(), &table.descriptor_table()));
    }

    #[test]
    fn table_admission_is_bounded_and_recovers() {
        let admission = super::TableAdmission::with_root();
        let permits = (1..super::TABLE_CAPACITY)
            .map(|_| admission.reserve().expect("capacity remains"))
            .collect::<Vec<_>>();
        assert!(admission.reserve().is_none());
        drop(permits.into_iter().next().unwrap());
        assert!(admission.reserve().is_some());
    }
}
