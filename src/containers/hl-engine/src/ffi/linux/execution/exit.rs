use std::sync::{Arc, OnceLock};

use hl_memory::MappingCoordinator;
use hl_time::RealtimeClock;

use super::{MappingHostAdapter, task};

pub(super) fn runtime(
    tasks: Arc<hl_task::TaskRegistry>,
    mappings: Arc<MappingCoordinator<MappingHostAdapter>>,
    descriptors: Arc<hl_runtime::DescriptorImageSlot>,
    epoll: Arc<hl_runtime::EpollControl>,
    futex: Arc<hl_runtime::SafeRuntimeFutex<MappingHostAdapter>>,
    catalog: Arc<hl_runtime::IpcCatalog>,
    ipc: Arc<hl_runtime::MemoryMappings<MappingHostAdapter>>,
    locks: Arc<hl_runtime::AdvisoryLockCoordinator>,
    clock: Arc<task::ClockIdentity>,
    vfork: Arc<OnceLock<Arc<hl_runtime::VforkParentToken>>>,
    handles: Arc<hl_runtime::ProcessHandleRegistry>,
    ptrace: Arc<hl_runtime::PtraceCatalog>,
    procfs_spaces: Arc<super::process_memory::ProcfsSpaces>,
    terminals: Option<Arc<hl_runtime::TerminalCatalog>>,
) -> Arc<hl_runtime::ExitRuntime> {
    let exit_clock = Arc::clone(&clock);
    let now = Arc::new(move || exit_clock.realtime_now().map(|value| value.seconds()).unwrap_or(0));
    Arc::new(hl_runtime::ExitRuntime::new(
        Arc::new(hl_runtime::RobustExitHandler::new(
            Arc::clone(&tasks),
            Arc::clone(&mappings),
            futex,
        )),
        Arc::new(hl_runtime::DescriptorExit::new(descriptors, epoll)),
        Arc::new(hl_runtime::IpcExitHandler::new(catalog, ipc, now)),
        Arc::new(VforkMemory {
            memory: hl_runtime::MemoryExit::new(mappings),
            procfs_spaces,
            vfork: Arc::clone(&vfork),
        }),
        Arc::new(hl_runtime::VfsLockExit::new(locks)),
        Arc::new(VforkFinalizer {
            finalizer: hl_runtime::RegistryExitFinalizer::new(Arc::clone(&tasks)),
            tasks,
            terminals,
            vfork,
            handles,
            ptrace,
        }),
    ))
}

struct VforkMemory {
    memory: hl_runtime::MemoryExit<MappingHostAdapter>,
    procfs_spaces: Arc<super::process_memory::ProcfsSpaces>,
    vfork: Arc<OnceLock<Arc<hl_runtime::VforkParentToken>>>,
}

struct BorrowedMemory;

impl hl_runtime::PreparedExitParticipant for BorrowedMemory {
    fn publish(&mut self) -> Result<(), hl_runtime::ExitRuntimeError> {
        Ok(())
    }

    fn rollback(&mut self) {}

    fn finish(&mut self) {}
}

impl hl_runtime::ExitParticipant for VforkMemory {
    fn prepare(
        &self,
        process: hl_task::ProcessId,
        threads: &[hl_task::ThreadId],
    ) -> Result<Box<dyn hl_runtime::PreparedExitParticipant>, hl_runtime::ExitRuntimeError> {
        self.procfs_spaces
            .capture_exit(process)
            .map_err(|_| hl_runtime::ExitRuntimeError::Failed)?;
        if self.vfork.get().is_some() {
            Ok(Box::new(BorrowedMemory))
        } else {
            hl_runtime::ExitParticipant::prepare(&self.memory, process, threads)
        }
    }
}

struct VforkFinalizer {
    finalizer: hl_runtime::RegistryExitFinalizer,
    tasks: Arc<hl_task::TaskRegistry>,
    terminals: Option<Arc<hl_runtime::TerminalCatalog>>,
    vfork: Arc<OnceLock<Arc<hl_runtime::VforkParentToken>>>,
    handles: Arc<hl_runtime::ProcessHandleRegistry>,
    ptrace: Arc<hl_runtime::PtraceCatalog>,
}

impl VforkFinalizer {
    fn release_terminal(&self, process: hl_task::ProcessId) {
        let Some(terminals) = &self.terminals else {
            return;
        };
        let snapshot = self.tasks.snapshot();
        let Some(session) = snapshot.sessions.iter().find(|session| session.leader == process) else {
            return;
        };
        let Ok(terminal) = terminals.controlling(session.id.number()) else {
            return;
        };
        if let Some(foreground) = terminal.foreground() {
            let hangup = hl_task::SignalNumber::new(1).expect("SIGHUP is a valid Linux signal");
            if let Some(group) = snapshot.process_groups.iter().find(|group| {
                group.session == session.id
                    && group.id.number() == foreground.number
                    && group.id.wire_parts().1 == foreground.generation
            }) {
                for member in &group.members {
                    let _ = self.tasks.enqueue_signal(
                        hl_task::PendingTarget::Process(*member),
                        hl_task::SignalInfo::bare(hangup),
                    );
                }
            }
        }
        let _ = terminals.detach(session.id.number(), terminal.id());
    }
}

impl hl_runtime::TaskExitFinalizer for VforkFinalizer {
    fn finalize(
        &self,
        process: hl_task::ProcessId,
        threads: &[hl_task::ThreadId],
        status: hl_task::ExitStatus,
    ) -> Result<(), hl_runtime::ExitRuntimeError> {
        self.release_terminal(process);
        hl_runtime::TaskExitFinalizer::finalize(&self.finalizer, process, threads, status)?;
        self.handles.notify_exit(process);
        self.ptrace.unregister(process);
        if let Some(token) = self.vfork.get() {
            let _ = token.release(process);
        }
        Ok(())
    }
}
