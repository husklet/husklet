use std::sync::Arc;

use hl_linux::ExecPlan;
use hl_task::{ProcessId, ThreadId};

use crate::{RuntimeExecError, RuntimeExecParticipant, RuntimeExecPort, SafeRuntimeExec};

/// Required exec domains. The discriminants are not publication ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Task,
    DescriptorEpoll,
    Ipc,
    Loader,
}

/// Complete, role-checked dependencies for one concrete exec coordinator.
pub struct RuntimeDependencies {
    task: Arc<dyn RuntimeExecParticipant>,
    descriptor_epoll: Arc<dyn RuntimeExecParticipant>,
    ipc: Arc<dyn RuntimeExecParticipant>,
    loader: Arc<dyn RuntimeExecParticipant>,
}

/// Incremental role assignment that rejects duplicate and incomplete wiring.
#[derive(Default)]
pub struct RuntimeDependenciesBuilder {
    task: Option<Arc<dyn RuntimeExecParticipant>>,
    descriptor_epoll: Option<Arc<dyn RuntimeExecParticipant>>,
    ipc: Option<Arc<dyn RuntimeExecParticipant>>,
    loader: Option<Arc<dyn RuntimeExecParticipant>>,
}

impl RuntimeDependencies {
    #[must_use]
    pub fn builder() -> RuntimeDependenciesBuilder {
        RuntimeDependenciesBuilder::default()
    }

    fn linux_orders(self) -> (Vec<Arc<dyn RuntimeExecParticipant>>, Vec<usize>) {
        (
            vec![self.task, self.descriptor_epoll, self.ipc, self.loader],
            vec![2, 1, 3, 0],
        )
    }
}

impl RuntimeDependenciesBuilder {
    pub fn participant(
        mut self,
        role: Role,
        participant: Arc<dyn RuntimeExecParticipant>,
    ) -> Result<Self, RuntimeExecError> {
        let slot = match role {
            Role::Task => &mut self.task,
            Role::DescriptorEpoll => &mut self.descriptor_epoll,
            Role::Ipc => &mut self.ipc,
            Role::Loader => &mut self.loader,
        };
        if slot.is_some() {
            return Err(RuntimeExecError::Invalid);
        }
        *slot = Some(participant);
        Ok(self)
    }

    pub fn build(self) -> Result<RuntimeDependencies, RuntimeExecError> {
        Ok(RuntimeDependencies {
            task: self.task.ok_or(RuntimeExecError::Unsupported)?,
            descriptor_epoll: self.descriptor_epoll.ok_or(RuntimeExecError::Unsupported)?,
            ipc: self.ipc.ok_or(RuntimeExecError::Unsupported)?,
            loader: self.loader.ok_or(RuntimeExecError::Unsupported)?,
        })
    }
}

/// Four-domain exec coordinator. Construction alone does not install the port.
pub struct Runtime {
    safe: SafeRuntimeExec,
}

impl Runtime {
    pub fn new(dependencies: RuntimeDependencies) -> Result<Self, RuntimeExecError> {
        let (participants, publish_order) = dependencies.linux_orders();
        Ok(Self {
            safe: SafeRuntimeExec::with_publish_order(participants, publish_order)?,
        })
    }
}

impl RuntimeExecPort for Runtime {
    fn prepare(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: ExecPlan,
    ) -> Result<Box<dyn crate::PreparedExec>, RuntimeExecError> {
        self.safe
            .prepare(process, thread, &plan)
            .map(|prepared| Box::new(prepared) as Box<dyn crate::PreparedExec>)
    }
}
