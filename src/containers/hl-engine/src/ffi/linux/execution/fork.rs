//! Process-fork composition and transactional publication.

use std::sync::Arc;

use hl_execution::{EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionSnapshot};
use hl_descriptor::{DescriptorFlags, StatusFlags};
use hl_linux::{ClonePlan, Errno, LinuxResult};
use hl_linux::{GuestAccess, GuestMemory};
use hl_runtime::{
    ForkArtifactExchange, ForkContext, ForkParticipant, ForkParticipantRole, IpcForkChild, IpcForkParticipant,
    MemoryChildMapping,
};
use hl_runtime::{PreparedThread, ProcessForkTrap, ThreadCloneTrap};
use hl_task::ThreadId;
use hl_task::{ForkCloneFlags, ForkRequest};

use super::{clone, readiness, routing, threads};

pub(super) struct Runtime {
    process: Arc<routing::ProcessContext>,
    threads: Arc<threads::ThreadSet>,
}

pub(super) struct Trap(pub(super) Arc<Runtime>, pub(super) ThreadId);

struct IpcForkGuard {
    participant: IpcForkParticipant<super::MappingHostAdapter>,
    context: ForkContext,
    reservation: u64,
    finished: bool,
}

impl IpcForkGuard {
    fn stage(
        parent: &routing::ProcessContext,
        child: hl_task::ProcessId,
        space: &super::space::AddressSpace,
    ) -> Result<Self, Errno> {
        let context = ForkContext {
            transaction: 1,
            request: ForkRequest {
                parent: parent.process().fork_identity(),
                child: child.fork_identity(),
                flags: ForkCloneFlags::default(),
            },
        };
        let participant = IpcForkParticipant::new(parent.ipc_catalog(), parent.ipc_mappings());
        let reservation = participant.prepare(context).map_err(|()| Errno::ENOMEM)?;
        let artifacts = ForkArtifactExchange::default();
        artifacts
            .publish(
                context,
                ForkParticipantRole::Memory,
                1,
                Arc::new(MemoryChildMapping(space.mappings())),
            )
            .map_err(|()| Errno::EIO)?;
        if participant
            .clone_with_artifacts(context, reservation, &artifacts)
            .and_then(|()| participant.commit(context, reservation))
            .is_err()
        {
            participant.rollback(context, reservation);
            return Err(Errno::ENOMEM);
        }
        Ok(Self {
            participant,
            context,
            reservation,
            finished: false,
        })
    }

    fn child(&self) -> Result<Arc<IpcForkChild<super::MappingHostAdapter>>, Errno> {
        self.participant.child(self.context.transaction).ok_or(Errno::EIO)
    }

    fn finish(mut self) -> Result<(), Errno> {
        self.participant
            .take_child(self.context.transaction)
            .ok_or(Errno::EIO)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for IpcForkGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.participant.rollback(self.context, self.reservation);
        }
    }
}

impl Runtime {
    const CHILD_CLEARTID: u64 = 0x0020_0000;
    const CHILD_SETTID: u64 = 0x0100_0000;
    const VM: u64 = 0x0000_0100;
    const VFORK: u64 = 0x0000_4000;
    const PIDFD: u64 = 0x0000_1000;

    pub(super) fn new(process: Arc<routing::ProcessContext>, threads: Arc<threads::ThreadSet>) -> Arc<Self> {
        Arc::new(Self { process, threads })
    }

    fn child_cpu(cpu: &ExecutionCpuSnapshot, stack: u64) -> ExecutionSnapshot {
        let mut cpu = cpu.clone();
        match &mut cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => {
                cpu.registers[0] = 0;
                if stack != 0 {
                    cpu.sp = stack;
                }
                cpu.clear_exclusive_reservation();
            }
            ExecutionCpuSnapshot::X86_64(cpu) => {
                cpu.registers[0] = 0;
                if stack != 0 {
                    cpu.registers[4] = stack;
                }
            }
        }
        ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu,
            cache_epoch: 1,
            fault: None,
        }
    }

    fn valid(plan: &ClonePlan) -> bool {
        let child_flags = Self::CHILD_CLEARTID | Self::CHILD_SETTID;
        let vfork = plan.flags & (Self::VM | Self::VFORK) == (Self::VM | Self::VFORK);
        let accepted = child_flags | Self::PIDFD | if vfork { Self::VM | Self::VFORK } else { 0 };
        plan.flags & !accepted == 0
            && plan.exit_signal == 17
            && plan.stack_size == 0
            && (plan.flags & child_flags == 0 || plan.child_tid != 0)
            && plan.set_tid == 0
            && plan.set_tid_count == 0
            && plan.cgroup == 0
    }

    fn spawn(&self, source: ThreadId, cpu: &ExecutionCpuSnapshot, linux: ClonePlan) -> Result<u32, Errno> {
        if !Self::valid(&linux) {
            return Err(Errno::EINVAL);
        }
        let tasks = self.process.tasks();
        let plan = tasks.begin_fork_process(source).map_err(|_| Errno::EAGAIN)?;
        let process = plan.process();
        let thread = plan.thread();
        let identity = process.fork_identity();
        let vfork = linux.flags & (Self::VM | Self::VFORK) == (Self::VM | Self::VFORK);
        let space = if vfork {
            self.process.space()
        } else {
            match self.process.space().fork_snapshot(hl_memory::AddressSpaceId {
                slot: u64::from(identity.slot),
                generation: u64::from(identity.generation),
            }) {
                Ok(value) => value,
                Err(error) => {
                    let _ = tasks.rollback_fork_process(plan);
                    return Err(match error {
                        super::space::Error::Capacity => Errno::EAGAIN,
                        super::space::Error::Memory => Errno::ENOSPC,
                        super::space::Error::Host => Errno::EIO,
                    });
                }
            }
        };
        let ipc = match IpcForkGuard::stage(&self.process, process, &space) {
            Ok(value) => value,
            Err(error) => {
                let _ = tasks.rollback_fork_process(plan);
                return Err(error);
            }
        };
        let context = match self
            .process
            .fork_child(source, process, Arc::clone(&space), ipc.child()?)
        {
            Ok(value) => value,
            Err(_) => {
                let _ = tasks.rollback_fork_process(plan);
                return Err(Errno::ENOMEM);
            }
        };
        if context.install_threads(&self.threads).is_err() {
            let _ = tasks.rollback_fork_process(plan);
            return Err(Errno::EIO);
        }
        if linux.flags & Self::CHILD_SETTID != 0 {
            let memory = context.memory();
            if memory.probe(linux.child_tid, 4, GuestAccess::Write) != Ok(4)
                || memory.write(linux.child_tid, &thread.number().to_le_bytes()) != Ok(4)
            {
                let _ = tasks.rollback_fork_process(plan);
                return Err(Errno::EFAULT);
            }
        }
        if linux.flags & Self::CHILD_CLEARTID != 0 && tasks.stage_fork_clear(&plan, linux.child_tid).is_err() {
            let _ = tasks.rollback_fork_process(plan);
            return Err(Errno::EIO);
        }
        let fork = Self::new(Arc::clone(&context), Arc::clone(&self.threads));
        if context.install_fork(&fork).is_err() {
            let _ = tasks.rollback_fork_process(plan);
            return Err(Errno::EIO);
        }
        let clones = Arc::new(clone::Contexts::new(Arc::clone(&context), Arc::clone(&self.threads)));
        let clone_runtime = clones.build();
        if clones.install(Arc::clone(&clone_runtime)).is_err() {
            let _ = tasks.rollback_fork_process(plan);
            return Err(Errno::EIO);
        }
        let cancellation = Arc::new(readiness::Cancellation::new().map_err(|_| Errno::ENOMEM)?);
        let clone = ThreadCloneTrap::new(clone_runtime, thread);
        let router = Arc::new(context.router(thread, Arc::clone(&cancellation), Some(Box::new(clone))));
        if let Err(error) = self.threads.prepare(thread, process, router, cancellation, space) {
            let _ = tasks.rollback_fork_process(plan);
            return Err(match error {
                hl_runtime::RuntimeThreadError::Capacity => Errno::EAGAIN,
                _ => Errno::EIO,
            });
        }
        let mut runnable = match self.threads.stage_fork(&plan, Self::child_cpu(cpu, linux.stack)) {
            Ok(value) => value,
            Err(_) => {
                self.threads.discard(thread);
                let _ = tasks.rollback_fork_process(plan);
                return Err(Errno::EIO);
            }
        };
        ipc.finish()?;
        if vfork {
            self.threads.park(source).map_err(|_| Errno::EIO)?;
            let wake: Arc<dyn hl_runtime::VforkWake> = self.threads.clone();
            let token = Arc::new(hl_runtime::VforkParentToken::new(source, process, wake));
            context.install_vfork(Arc::clone(&token)).map_err(|_| Errno::EIO)?;
        }
        let parent_files = self.process.files(source);
        let parent_descriptors = parent_files.descriptor_table();
        let pidfd = if linux.flags & Self::PIDFD != 0 {
            let object = hl_runtime::ProcessHandleRegistry::create(process);
            let install = parent_descriptors
                .prepare_open(
                    0,
                    object.clone(),
                    StatusFlags::default(),
                    DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
                )
                .map_err(|_| Errno::EMFILE)?;
            self.process
                .handles()
                .register(install.description_identity(), object)
                .map_err(|_| Errno::ENFILE)?;
            let mut previous = [0_u8; 4];
            let memory = self.process.memory();
            if memory.read(linux.pidfd, &mut previous) != Ok(4)
                || memory.write(linux.pidfd, &install.number().to_le_bytes()) != Ok(4)
            {
                return Err(Errno::EFAULT);
            }
            Some((install, previous))
        } else {
            None
        };
        if runnable.activate_fork(&plan).is_err() {
            if let Some((_, previous)) = &pidfd {
                let _ = self.process.memory().write(linux.pidfd, previous);
            }
            let _ = tasks.rollback_fork_process(plan);
            return Err(Errno::EIO);
        }
        context.publish_procfs();
        self.process.observe_fork();
        if let Some((install, _)) = pidfd {
            install.publish();
        }
        Box::new(runnable).publish();
        Ok(process.number())
    }
}

impl ProcessForkTrap for Trap {
    fn fork(&self, cpu: &ExecutionCpuSnapshot, plan: ClonePlan) -> LinuxResult {
        match self.0.spawn(self.1, cpu, plan) {
            Ok(process) => LinuxResult::Value(u64::from(process)),
            Err(error) => LinuxResult::Error(error),
        }
    }
}
