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
        let Some(terminals) = &self.terminals else { return };
        release_terminal(&self.tasks, terminals, process);
    }
}

fn release_terminal(
    tasks: &hl_task::TaskRegistry,
    terminals: &hl_runtime::TerminalCatalog,
    process: hl_task::ProcessId,
) {
    let Ok(prepared) = tasks.prepare_terminal_transition(process, hl_task::TerminalTransition::SessionLeaderExit)
    else {
        return;
    };
    let session = prepared.effects().session;
    let Ok(terminal) = terminals.controlling(session.number()) else {
        return;
    };
    let foreground = terminal.foreground().and_then(|group| {
        group
            .number
            .checked_sub(1)
            .and_then(|slot| hl_task::ProcessGroupId::from_wire(slot, group.generation))
    });
    let prepared = prepared.target_foreground(foreground);
    if terminals.detach(session.number(), terminal.id()).is_ok() {
        prepared.commit();
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

#[cfg(test)]
mod tests {
    use super::release_terminal;
    use hl_task::{
        ProcessCredentials, ProcessLimits, RegistryConfig, SignalAction, SignalDisposition, SignalNumber, TaskRegistry,
    };

    fn fixture() -> (TaskRegistry, hl_task::ProcessId, hl_task::ThreadId) {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (_init, init_thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        let leader_plan = tasks.begin_fork_process(init_thread).unwrap();
        let leader = leader_plan.process();
        let leader_thread = leader_plan.thread();
        tasks.commit_fork_process(leader_plan).unwrap();
        tasks.create_session(leader).unwrap();
        (tasks, leader, leader_thread)
    }

    #[test]
    fn session_leader_exit_hangs_up_foreground_and_detaches_terminal() {
        let (tasks, leader, leader_thread) = fixture();
        let worker_plan = tasks.begin_fork_process(leader_thread).unwrap();
        let worker = worker_plan.process();
        let worker_thread = worker_plan.thread();
        tasks.commit_fork_process(worker_plan).unwrap();
        let foreground = tasks.set_process_group(leader, worker, None).unwrap();
        tasks.set_foreground_group(leader, foreground).unwrap();
        tasks
            .set_action(
                worker,
                SignalNumber::new(1).unwrap(),
                SignalAction {
                    disposition: SignalDisposition::Handler(0x4000),
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();

        let terminals = hl_runtime::TerminalCatalog::default();
        let terminal = terminals.allocate().unwrap();
        let session = tasks.session_id(leader).unwrap();
        terminals.acquire(session.number(), terminal.id()).unwrap();
        tasks.attach_terminal(leader, session).unwrap();
        let (_, generation) = foreground.wire_parts();
        terminal
            .set_foreground(hl_runtime::TerminalForegroundGroup {
                number: foreground.number(),
                generation,
            })
            .unwrap();

        release_terminal(&tasks, &terminals, leader);

        assert!(terminals.controlling(session.number()).is_err());
        assert_eq!(tasks.pending_signal_mask(worker_thread).unwrap().bits(), 1);
    }

    #[test]
    fn nonleader_exit_preserves_controlling_terminal() {
        let (tasks, leader, leader_thread) = fixture();
        let child_plan = tasks.begin_fork_process(leader_thread).unwrap();
        let child = child_plan.process();
        tasks.commit_fork_process(child_plan).unwrap();
        let terminals = hl_runtime::TerminalCatalog::default();
        let terminal = terminals.allocate().unwrap();
        let session = tasks.session_id(leader).unwrap();
        terminals.acquire(session.number(), terminal.id()).unwrap();
        tasks.attach_terminal(leader, session).unwrap();

        release_terminal(&tasks, &terminals, child);

        assert_eq!(terminals.controlling(session.number()).unwrap().id(), terminal.id());
    }

    #[test]
    fn failed_terminal_detach_does_not_publish_exit_transition() {
        let (tasks, leader, leader_thread) = fixture();
        let worker_plan = tasks.begin_fork_process(leader_thread).unwrap();
        let worker = worker_plan.process();
        let worker_thread = worker_plan.thread();
        tasks.commit_fork_process(worker_plan).unwrap();
        let foreground = tasks.set_process_group(leader, worker, None).unwrap();
        tasks.set_foreground_group(leader, foreground).unwrap();
        tasks
            .set_action(
                worker,
                SignalNumber::new(1).unwrap(),
                SignalAction {
                    disposition: SignalDisposition::Handler(0x4000),
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();
        let terminals = hl_runtime::TerminalCatalog::default();
        let terminal = terminals.allocate().unwrap();
        let session = tasks.session_id(leader).unwrap();
        terminals.acquire(session.number(), terminal.id()).unwrap();
        tasks.attach_terminal(leader, session).unwrap();
        terminals.detach(session.number(), terminal.id()).unwrap();

        release_terminal(&tasks, &terminals, leader);

        assert_eq!(tasks.pending_signal_mask(worker_thread).unwrap().bits(), 0);
        assert_eq!(tasks.terminal_session(leader).unwrap(), Some(session));
    }

    #[test]
    fn stale_terminal_foreground_does_not_block_leader_exit_detach() {
        let (tasks, leader, leader_thread) = fixture();
        let worker_plan = tasks.begin_fork_process(leader_thread).unwrap();
        let worker = worker_plan.process();
        let worker_thread = worker_plan.thread();
        tasks.commit_fork_process(worker_plan).unwrap();
        let foreground = tasks.set_process_group(leader, worker, None).unwrap();
        tasks.set_foreground_group(leader, foreground).unwrap();
        tasks
            .set_action(
                worker,
                SignalNumber::new(1).unwrap(),
                SignalAction {
                    disposition: SignalDisposition::Handler(0x4000),
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();
        let terminals = hl_runtime::TerminalCatalog::default();
        let terminal = terminals.allocate().unwrap();
        let session = tasks.session_id(leader).unwrap();
        terminals.acquire(session.number(), terminal.id()).unwrap();
        tasks.attach_terminal(leader, session).unwrap();
        let (_, generation) = foreground.wire_parts();
        terminal
            .set_foreground(hl_runtime::TerminalForegroundGroup {
                number: foreground.number(),
                generation: generation.saturating_add(1),
            })
            .unwrap();

        release_terminal(&tasks, &terminals, leader);

        assert!(terminals.controlling(session.number()).is_err());
        assert_eq!(tasks.pending_signal_mask(worker_thread).unwrap().bits(), 0);
    }
}
