//! Process-context accessors and exec image staging.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use hl_isa::GuestAddress;
use hl_runtime::{BrkRegion, BrkSnapshot, RuntimeMemorySyscalls, RuntimeSyscallRouter};

use super::ProcessContext;
use super::image::SignalGateway;
use crate::engine::EngineError;

impl ProcessContext {
    pub(in crate::ffi::linux::execution) fn auxiliary_slot(&self) -> Result<Arc<Mutex<Vec<u8>>>, EngineError> {
        self.path_host
            .as_ref()
            .map(|host| host.auxiliary_slot())
            .ok_or(EngineError::LaunchFailed)
    }

    pub(in crate::ffi::linux::execution) fn set_auxiliary(&self, bytes: Vec<u8>) -> Result<(), EngineError> {
        self.path_host
            .as_ref()
            .ok_or(EngineError::LaunchFailed)?
            .set_auxiliary(bytes);
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn tasks(&self) -> Arc<hl_task::TaskRegistry> {
        Arc::clone(&self.tasks)
    }

    pub(in crate::ffi::linux::execution) fn ipc_catalog(&self) -> Arc<hl_runtime::IpcCatalog> {
        Arc::clone(&self.ipc_catalog)
    }

    pub(in crate::ffi::linux::execution) fn ipc_mappings(
        &self,
    ) -> Arc<hl_runtime::MemoryMappings<super::super::MappingHostAdapter>> {
        Arc::clone(&self.ipc)
    }

    pub(in crate::ffi::linux::execution) fn space(&self) -> Arc<super::super::space::AddressSpace> {
        Arc::clone(&self.space)
    }

    pub(in crate::ffi::linux::execution) fn stage_exec(
        &self,
        thread: hl_task::ThreadId,
        plan: &hl_linux::ExecPlan,
    ) -> Result<super::super::path::ExecTarget, hl_runtime::RuntimeExecError> {
        let host = self
            .path_host
            .as_ref()
            .ok_or(hl_runtime::RuntimeExecError::Unsupported)?;
        if self.projected {
            return host.stage_projected(plan);
        }
        if !plan.path.is_empty() {
            return self.stage_path(thread, host, plan);
        }
        if plan.flags & 0x1000 == 0 {
            return host.stage_exec(plan, &self.working.snapshot().path);
        }
        let descriptor = plan.directory.ok_or(hl_runtime::RuntimeExecError::BadDescriptor)?;
        let lease = self
            .files(thread)
            .descriptor_table()
            .pin(descriptor)
            .map_err(|_| hl_runtime::RuntimeExecError::BadDescriptor)?;
        let metadata = lease
            .metadata()
            .map_err(|_| hl_runtime::RuntimeExecError::BadDescriptor)?;
        let mut resolved = plan.clone();
        resolved.path = host.descriptor_exec(&metadata)?;
        resolved.directory = Some(-100);
        resolved.flags = 0;
        let mut target = host.stage_exec(&resolved, &self.working.snapshot().path)?;
        target.execfn = format!("/dev/fd/{descriptor}").into_bytes();
        Ok(target)
    }

    fn stage_path(
        &self,
        thread: hl_task::ThreadId,
        host: &super::super::path::NativePath,
        plan: &hl_linux::ExecPlan,
    ) -> Result<super::super::path::ExecTarget, hl_runtime::RuntimeExecError> {
        if plan.path.starts_with(b"/") || plan.directory.is_none_or(|value| value == -100) {
            return host.stage_exec(plan, &self.working.snapshot().path);
        }
        let descriptor = plan.directory.ok_or(hl_runtime::RuntimeExecError::BadDescriptor)?;
        let lease = self
            .files(thread)
            .descriptor_table()
            .pin(descriptor)
            .map_err(|_| hl_runtime::RuntimeExecError::BadDescriptor)?;
        let metadata = lease
            .metadata()
            .map_err(|_| hl_runtime::RuntimeExecError::BadDescriptor)?;
        let mut resolved = plan.clone();
        let mut path = host.descriptor_exec(&metadata)?;
        if !path.ends_with(b"/") {
            path.push(b'/');
        }
        path.extend_from_slice(&plan.path);
        resolved.path = path;
        resolved.directory = Some(-100);
        host.stage_exec(&resolved, &self.working.snapshot().path)
    }

    pub(in crate::ffi::linux::execution) const fn process(&self) -> hl_task::ProcessId {
        self.process
    }

    pub(in crate::ffi::linux::execution) fn epoll(&self) -> Arc<hl_runtime::Control> {
        Arc::clone(&self.epoll)
    }

    pub(in crate::ffi::linux::execution) fn install_exec(
        &self,
        coordinator: &Arc<crate::ffi::linux::execution::exec_image::Coordinator>,
    ) -> Result<(), ()> {
        self.exec_coordinator.set(Arc::downgrade(coordinator)).map_err(|_| ())
    }

    pub(in crate::ffi::linux::execution) fn install_registration(
        &self,
        registration: Arc<crate::ffi::linux::execution::exec_image::Registration>,
    ) -> Result<(), ()> {
        self.exec_registration.set(registration).map_err(|_| ())
    }

    pub(in crate::ffi::linux::execution) fn from_candidate(
        &self,
        source_files: Arc<hl_runtime::RuntimeDescriptorTable>,
        table: Arc<hl_descriptor::DescriptorTable>,
        space: Arc<super::super::space::AddressSpace>,
        executable: Vec<u8>,
        auxiliary: Vec<u8>,
    ) -> Result<Arc<Self>, EngineError> {
        let descriptors = Arc::new(self.descriptors.fork(Arc::clone(&table)));
        let source_files = Arc::new(self.epoll.exec_image(&source_files, Arc::clone(&table)));
        let mappings = space.mappings();
        let process_memory = space.guest_memory();
        // Exec replaces the address-space image, not the open file
        // descriptions. Keep the process memfd registry so descriptors that
        // survive the CLOEXEC sweep can still resolve their shared backing in
        // the new image.
        let (memfds, anonymous_account) = {
            let memory = self.memory.lock().map_err(|_| EngineError::Synchronization)?;
            (memory.memfd_registry(), memory.anonymous_account())
        };
        let mut brk = BrkRegion::new(
            Arc::clone(&mappings),
            BrkSnapshot {
                lower: GuestAddress::new(0x80_0000),
                current: GuestAddress::new(0x80_0000),
                upper: GuestAddress::new(0xf0_0000),
                backing_identity: hl_runtime::BRK_BACKING_IDENTITY,
            },
        )
        .map_err(|_| EngineError::LaunchFailed)?;
        if let Some(account) = anonymous_account {
            brk = brk.with_account(account).map_err(|_| EngineError::LaunchFailed)?;
        }
        let mut memory = RuntimeMemorySyscalls::new(
            Arc::clone(&mappings),
            Arc::clone(&table),
            process_memory,
            self.architecture,
        )
        .with_process(self.process.number())
        .with_brk(brk)
        .with_address_limit(space.arena().length() as u64)
        .with_host(Arc::new(super::super::super::memory_control::Control::new(
            space.arena(),
            Arc::clone(&mappings),
            Arc::new(super::super::memory_limit::MemoryLimit::new(
                Arc::clone(&self.tasks),
                self.process,
            )),
        )));
        if let Some(shared) = mappings.shared_objects() {
            memory = memory.with_memfd_objects(Arc::clone(&shared), u64::from(self.process.number()), memfds);
            if let Some(host) = &self.path_host {
                memory = memory.with_descriptor_source(Arc::new(host.mapping_source(space.arena())));
            }
        }
        let interruptions = Arc::new(super::super::task::FutexInterrupt::new());
        // Exec installs a new address space in the same process. Its shared
        // mappings must remain in the existing futex namespace so waiters in
        // the new image can be woken through another process's mapping of the
        // same backing object.
        let futex = Arc::new(
            self.futex
                .fork(Arc::clone(&mappings), interruptions.clone())
                .map_err(|_| EngineError::LaunchFailed)?,
        );
        let sigreturn_pc = SignalGateway::install(&space.mappings(), &space.guest_memory(), self.architecture)?;
        let ipc = Arc::new(hl_runtime::MemoryMappings::new(Arc::clone(&mappings)));
        let descriptor_image = source_files.image_slot();
        let vfork = Arc::new(OnceLock::new());
        let exit = super::exit_runtime(
            Arc::clone(&self.tasks),
            mappings,
            Arc::clone(&descriptor_image),
            Arc::clone(&self.epoll),
            futex.clone(),
            Arc::clone(&self.ipc_catalog),
            Arc::clone(&ipc),
            Arc::clone(&self.locks),
            Arc::clone(&self.clock),
            Arc::clone(&vfork),
            Arc::clone(&self.handles),
            Arc::clone(&self.ptrace),
            Arc::clone(&self.procfs_spaces),
            self.path_host.as_ref().map(|host| host.terminal_catalog()),
        );
        let trace_exchange = hl_runtime::TraceExchange::new(Arc::new(space.guest_memory()));
        self.ptrace.register(self.process, trace_exchange);
        let candidate = Arc::new(Self {
            projected: self.projected,
            aio: Arc::new(hl_runtime::AioCatalog::default()),
            descriptors,
            entropy: Arc::clone(&self.entropy),
            handles: Arc::clone(&self.handles),
            namespace_handles: Arc::clone(&self.namespace_handles),
            events: Arc::clone(&self.events),
            epoll: Arc::clone(&self.epoll),
            epoll_table: source_files,
            table_admission: Arc::clone(&self.table_admission),
            _table_permit: self._table_permit.clone(),
            thread_files: Mutex::new(std::collections::BTreeMap::new()),
            network: Arc::clone(&self.network),
            network_enabled: self.network_enabled,
            event_operations: Arc::clone(&self.event_operations),
            event_checkpoint: self.event_checkpoint.clone(),
            tasks: Arc::clone(&self.tasks),
            seccomp: Arc::clone(&self.seccomp),
            seccomp_baseline: self.seccomp_baseline,
            process: self.process,
            memory: Arc::new(Mutex::new(memory)),
            clock: Arc::clone(&self.clock),
            deadlines: Arc::clone(&self.deadlines),
            alarms: Arc::clone(&self.alarms),
            timers: Arc::clone(&self.timers),
            exec: Arc::clone(&self.exec),
            exec_queue: Arc::clone(&self.exec_queue),
            futex,
            interruptions,
            architecture: self.architecture,
            trace: self.trace,
            path_host: self
                .path_host
                .as_ref()
                .map(|host| host.exec_image(executable, auxiliary)),
            watches: self.watches.clone(),
            space,
            procfs_spaces: Arc::clone(&self.procfs_spaces),
            procfs_resources: Arc::clone(&self.procfs_resources),
            fork: OnceLock::new(),
            threads: OnceLock::new(),
            clone_context: OnceLock::new(),
            exec_coordinator: OnceLock::new(),
            sigreturn_pc,
            exec_registration: OnceLock::new(),
            working: Arc::clone(&self.working),
            fs_context: Arc::clone(&self.fs_context),
            ipc_catalog: Arc::clone(&self.ipc_catalog),
            posix_queues: Arc::clone(&self.posix_queues),
            ipc_pipes: Arc::clone(&self.ipc_pipes),
            ipc,
            locks: Arc::clone(&self.locks),
            exit,
            ptrace: Arc::clone(&self.ptrace),
            system: Arc::clone(&self.system),
            vfork,
        });
        if let Some(coordinator) = self.exec_coordinator.get().and_then(Weak::upgrade) {
            candidate
                .install_exec(&coordinator)
                .map_err(|()| EngineError::LaunchFailed)?;
        }
        if let Some(registration) = self.exec_registration.get() {
            candidate
                .install_registration(Arc::clone(registration))
                .map_err(|()| EngineError::LaunchFailed)?;
        }
        Ok(candidate)
    }

    pub(in crate::ffi::linux::execution) fn bind_candidate(
        self: &Arc<Self>,
        threads: &Arc<super::super::threads::ThreadSet>,
        thread: hl_task::ThreadId,
        cancellation: Arc<super::super::readiness::Cancellation>,
    ) -> Result<Arc<RuntimeSyscallRouter>, EngineError> {
        let contexts = Arc::new(super::super::clone::Contexts::new(
            Arc::clone(self),
            Arc::clone(threads),
        ));
        let clone_runtime = contexts.build();
        contexts
            .install(Arc::clone(&clone_runtime))
            .map_err(|()| EngineError::LaunchFailed)?;
        self.install_clone(&contexts).map_err(|()| EngineError::LaunchFailed)?;
        let fork_runtime = super::super::fork::Runtime::new(Arc::clone(self), Arc::clone(threads));
        self.install_fork(&fork_runtime)
            .map_err(|()| EngineError::LaunchFailed)?;
        self.install_threads(threads).map_err(|()| EngineError::LaunchFailed)?;
        let trap = hl_runtime::ThreadCloneTrap::new(clone_runtime, thread);
        Ok(Arc::new(self.router(thread, cancellation, Some(Box::new(trap)))))
    }

    pub(in crate::ffi::linux::execution) fn publish_procfs(&self) {
        self.procfs_spaces.publish(self.process, &self.space);
        self.procfs_resources
            .publish(self.process, &self.epoll_table.descriptor_table(), &self.working);
    }
}
