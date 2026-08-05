use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use hl_linux::ClonePlan;
use hl_memory::MappingHost;
use hl_task::{ForkCloneFlags, ProcessId, TaskRegistry, ThreadId};

use crate::{
    Control, DescriptorForkParticipant, EventForkParticipant, ExecutionForkParticipant, ForkCancellation,
    ForkChildResourceCatalog, ForkChildResources, ForkCoordinator, ForkParticipant, IpcForkParticipant, MemoryForkHost,
    MemoryForkParticipant, NetworkForkParticipant, PrivateFutexReset, ProviderForkParticipant, RuntimeForkError,
    RuntimeForkPort, RuntimeForkResult, TaskForkParticipant,
};

pub struct RuntimeDependencies<H, F, R>
where
    H: MappingHost,
{
    pub tasks: Arc<TaskRegistry>,
    pub epoll: Arc<Control>,
    pub resources: Arc<ForkChildResourceCatalog<H>>,
    pub initial: Arc<ForkChildResources<H>>,
    pub memory_host: Arc<F>,
    pub futex_reset: Arc<R>,
}

pub struct Runtime<H, F, R>
where
    H: MappingHost,
{
    tasks: Arc<TaskRegistry>,
    epoll: Arc<Control>,
    resources: Arc<ForkChildResourceCatalog<H>>,
    initial: Arc<ForkChildResources<H>>,
    memory_host: Arc<F>,
    futex_reset: Arc<R>,
    #[cfg(test)]
    fault: Mutex<Option<(crate::ForkParticipantRole, crate::ForkPhase)>>,
}

impl<H, F, R> Runtime<H, F, R>
where
    H: MappingHost,
{
    pub fn new(dependencies: RuntimeDependencies<H, F, R>) -> Result<Self, RuntimeForkError> {
        let snapshot = dependencies.tasks.snapshot();
        if !snapshot.processes.iter().any(|process| {
            process.id == dependencies.initial.process && process.threads.contains(&dependencies.initial.thread)
        }) || !Arc::ptr_eq(&dependencies.initial.memory, &dependencies.initial.ipc.memory)
        {
            return Err(RuntimeForkError::Invalid);
        }
        Ok(Self {
            tasks: dependencies.tasks,
            epoll: dependencies.epoll,
            resources: dependencies.resources,
            initial: dependencies.initial,
            memory_host: dependencies.memory_host,
            futex_reset: dependencies.futex_reset,
            #[cfg(test)]
            fault: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_fault(&self, role: crate::ForkParticipantRole, phase: crate::ForkPhase) {
        *self.fault.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((role, phase));
    }

    #[cfg(test)]
    pub(crate) fn clear_fault(&self) {
        *self.fault.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    #[cfg(test)]
    fn participant(&self, participant: Arc<dyn ForkParticipant>) -> Arc<dyn ForkParticipant> {
        Arc::new(TestFaultParticipant {
            inner: participant,
            fault: *self.fault.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        })
    }

    fn parent(&self, process: ProcessId, thread: ThreadId) -> Result<Arc<ForkChildResources<H>>, RuntimeForkError> {
        let resources = if process == self.initial.process {
            Arc::clone(&self.initial)
        } else {
            self.resources.child(process).ok_or(RuntimeForkError::Invalid)?
        };
        if resources.thread != thread {
            return Err(RuntimeForkError::Invalid);
        }
        Ok(resources)
    }

    fn flags(plan: ClonePlan) -> Result<ForkCloneFlags, RuntimeForkError> {
        const VM: u64 = 0x100;
        const FILES: u64 = 0x400;
        const SIGHAND: u64 = 0x800;
        const VFORK: u64 = 0x4000;
        let supported = VM | FILES | SIGHAND | VFORK;
        if plan.flags & !supported != 0
            || (plan.flags & VFORK != 0 && plan.flags & VM == 0)
            || plan.stack != 0
            || plan.stack_size != 0
            || plan.parent_tid != 0
            || plan.child_tid != 0
            || plan.tls != 0
            || plan.pidfd != 0
            || plan.set_tid != 0
            || plan.set_tid_count != 0
            || plan.cgroup != 0
            || plan.exit_signal != 17
        {
            return Err(RuntimeForkError::Unsupported);
        }
        let mut flags = ForkCloneFlags::default();
        for (bit, domain) in [
            (VM, ForkCloneFlags::VM),
            (FILES, ForkCloneFlags::FILES),
            (SIGHAND, ForkCloneFlags::SIGHAND),
        ] {
            if plan.flags & bit != 0 {
                flags = flags.union(domain);
            }
        }
        flags.validate().map_err(|_| RuntimeForkError::Invalid)?;
        Ok(flags)
    }
}

struct NeverCancel;

impl ForkCancellation for NeverCancel {
    fn cancelled(&self) -> bool {
        false
    }
}

impl<H, F, R> RuntimeForkPort for Runtime<H, F, R>
where
    H: MappingHost + 'static,
    F: MemoryForkHost<H> + 'static,
    R: PrivateFutexReset + 'static,
{
    fn fork(
        &self,
        parent: ProcessId,
        thread: ThreadId,
        plan: ClonePlan,
    ) -> Result<RuntimeForkResult, RuntimeForkError> {
        let flags = Self::flags(plan)?;
        let source = self.parent(parent, thread)?;
        let task = Arc::new(
            TaskForkParticipant::reserve_deferred(Arc::clone(&self.tasks), thread)
                .map_err(|_| RuntimeForkError::Again)?,
        );
        let (child_process, _) = task.reserved_child().ok_or(RuntimeForkError::Failed)?;
        let resource_reservation = self
            .resources
            .prepare(child_process)
            .map_err(|_| RuntimeForkError::Again)?;
        let descriptors = Arc::new(DescriptorForkParticipant::new(
            Arc::clone(&self.epoll),
            Arc::clone(&source.descriptors),
        ));
        let memory = Arc::new(MemoryForkParticipant::new(
            Arc::clone(&source.memory),
            Arc::clone(&self.memory_host),
            Arc::clone(&self.futex_reset),
        ));
        let providers = Arc::new(ProviderForkParticipant::new(Arc::clone(&source.providers)));
        let execution = Arc::new(ExecutionForkParticipant::new(Arc::clone(&source.execution)));
        let network = Arc::new(NetworkForkParticipant::new(Arc::clone(&source.network)));
        let event = Arc::new(EventForkParticipant::new(Arc::clone(&source.event)));
        let ipc = Arc::new(IpcForkParticipant::new(
            Arc::clone(&source.ipc.catalog),
            source.ipc.mappings.clone(),
        ));
        let participants: Vec<Arc<dyn ForkParticipant>> = vec![
            task.clone(),
            descriptors.clone(),
            memory.clone(),
            providers.clone(),
            execution.clone(),
            network.clone(),
            event.clone(),
            ipc.clone(),
        ];
        #[cfg(test)]
        let participants = participants
            .into_iter()
            .map(|participant| self.participant(participant))
            .collect();
        let request = hl_task::ForkRequest {
            flags,
            ..task.request()
        };
        let outcome = ForkCoordinator::new(participants)
            .map_err(|_| RuntimeForkError::Failed)?
            .fork(request, &NeverCancel)
            .map_err(|_| RuntimeForkError::Failed)?;
        let transaction = outcome.context.transaction;
        let ipc_child = ipc.take_child(transaction).ok_or(RuntimeForkError::Failed)?;
        let child = ForkChildResources {
            process: child_process,
            thread: task.reserved_child().ok_or(RuntimeForkError::Failed)?.1,
            descriptors: descriptors.take_child(transaction).ok_or(RuntimeForkError::Failed)?,
            memory: memory.take_child(transaction).ok_or(RuntimeForkError::Failed)?,
            providers: providers.take_child(transaction).ok_or(RuntimeForkError::Failed)?,
            execution: execution.take_child(transaction).ok_or(RuntimeForkError::Failed)?,
            network: network.take_child(transaction).ok_or(RuntimeForkError::Failed)?,
            event: event.take_child(transaction).ok_or(RuntimeForkError::Failed)?,
            ipc: ipc_child,
        };
        let ready = resource_reservation
            .stage(child)
            .map_err(|_| RuntimeForkError::Failed)?;
        let (process, thread) = task.publish_deferred().map_err(|_| RuntimeForkError::Failed)?;
        ready.publish();
        Ok(RuntimeForkResult { process, thread })
    }
}

#[cfg(test)]
struct TestFaultParticipant {
    inner: Arc<dyn ForkParticipant>,
    fault: Option<(crate::ForkParticipantRole, crate::ForkPhase)>,
}

#[cfg(test)]
impl TestFaultParticipant {
    fn fails(&self, phase: crate::ForkPhase) -> bool {
        self.fault == Some((self.inner.role(), phase))
    }
}

#[cfg(test)]
impl ForkParticipant for TestFaultParticipant {
    fn role(&self) -> crate::ForkParticipantRole {
        self.inner.role()
    }
    fn prepare(&self, context: crate::ForkContext) -> Result<u64, ()> {
        if self.fails(crate::ForkPhase::Prepare) {
            Err(())
        } else {
            self.inner.prepare(context)
        }
    }
    fn freeze(&self, context: crate::ForkContext, reservation: u64) -> Result<(), ()> {
        if self.fails(crate::ForkPhase::Freeze) {
            Err(())
        } else {
            self.inner.freeze(context, reservation)
        }
    }
    fn clone_parent(&self, context: crate::ForkContext, reservation: u64) -> Result<(), ()> {
        if self.fails(crate::ForkPhase::CloneParent) {
            Err(())
        } else {
            self.inner.clone_parent(context, reservation)
        }
    }
    fn clone_child(&self, context: crate::ForkContext, reservation: u64) -> Result<(), ()> {
        if self.fails(crate::ForkPhase::CloneChild) {
            Err(())
        } else {
            self.inner.clone_child(context, reservation)
        }
    }
    fn clone_with_artifacts(
        &self,
        context: crate::ForkContext,
        reservation: u64,
        artifacts: &crate::ForkArtifactExchange,
    ) -> Result<(), ()> {
        if self.fails(crate::ForkPhase::CloneChild) {
            Err(())
        } else {
            self.inner.clone_with_artifacts(context, reservation, artifacts)
        }
    }
    fn repair_parent(&self, context: crate::ForkContext, reservation: u64) -> Result<(), ()> {
        if self.fails(crate::ForkPhase::RepairParent) {
            Err(())
        } else {
            self.inner.repair_parent(context, reservation)
        }
    }
    fn repair_child(&self, context: crate::ForkContext, reservation: u64) -> Result<(), ()> {
        if self.fails(crate::ForkPhase::RepairChild) {
            Err(())
        } else {
            self.inner.repair_child(context, reservation)
        }
    }
    fn commit(&self, context: crate::ForkContext, reservation: u64) -> Result<(), ()> {
        if self.fails(crate::ForkPhase::Commit) {
            Err(())
        } else {
            self.inner.commit(context, reservation)
        }
    }
    fn rollback(&self, context: crate::ForkContext, reservation: u64) {
        self.inner.rollback(context, reservation);
    }
}
