//! Instance-owned construction of the runtime domains available today.

use crate::{
    CheckpointError, CheckpointIpcCatalog, CheckpointTaskRegistry, Control, ControlError, ExitParticipant,
    RuntimeCheckpointCoordinator, RuntimeDescriptorTable, RuntimeExecPort, RuntimeForkPort, SeccompControl,
};
use hl_event::{EventCatalog, EventCatalogError};
use hl_ipc::{
    IpcCatalog, MessageLimits, MessageQueueNamespace, SemaphoreLimits, SemaphoreNamespace, SharedMemoryLimits,
    SharedMemoryNamespace,
};
use hl_memory::SharedObjectStore;
use hl_network::NetworkCatalog;
use hl_provider::{HandleNamespace, NamespaceError};
use hl_task::{TaskError, TaskRegistry};
use hl_vfs::AdvisoryLockCoordinator;
use std::sync::{Arc, Mutex};

mod checkpoint;
mod construction;
mod exec;

pub use exec::ExecSlot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostCapacityPlan {
    pub maximum_processes: usize,
    pub maximum_threads: usize,
    pub maximum_groups: usize,
    pub maximum_pending_signals: usize,
    pub maximum_seccomp_threads: usize,
    pub descriptor_limit: i32,
    pub epoll_graph_limit: usize,
    pub event_capacity: usize,
    pub provider_capacity: usize,
}

impl Default for HostCapacityPlan {
    fn default() -> Self {
        Self {
            maximum_processes: 1024,
            maximum_threads: 4096,
            maximum_groups: 32,
            maximum_pending_signals: 1024,
            maximum_seccomp_threads: 4096,
            descriptor_limit: 65_536,
            epoll_graph_limit: 4096,
            event_capacity: 4096,
            provider_capacity: 4096,
        }
    }
}

/// Compatibility name for the instance-owned host capacity plan.
pub type RuntimeAssemblyConfig = HostCapacityPlan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDomain {
    Task,
    DescriptorEvent,
    EventCatalog,
    Provider,
    Seccomp,
    Memory,
    Network,
    Ipc,
    Linux,
    Loader,
    Execution,
    Checkpoint,
    Fork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeAssemblyError {
    Construction(RuntimeDomain),
    Unsupported(RuntimeDomain),
}

#[derive(Debug)]
pub enum AssemblyCheckpointError {
    Missing,
    Unsupported(RuntimeDomain),
    Transaction(CheckpointError),
}

/// Concrete foundational graph. Option fields permit explicit reverse teardown.
pub struct RuntimeAssembly {
    tasks: Option<Arc<CheckpointTaskRegistry>>,
    epoll: Option<Arc<Control>>,
    descriptors: Option<Arc<RuntimeDescriptorTable>>,
    checkpoint_descriptors: Option<Arc<crate::CheckpointDescriptorTable>>,
    events: Option<Arc<crate::CheckpointEventCatalog>>,
    event_resources: Option<Arc<crate::EventResourceRegistry>>,
    event_bindings: Option<Arc<crate::EventObjectBindings>>,
    network: Option<Arc<crate::CheckpointNetworkCatalog>>,
    providers: Option<Arc<crate::CheckpointProviderNamespace>>,
    provider_registry: Option<Arc<crate::ProviderRegistry>>,
    seccomp: Option<Arc<SeccompControl>>,
    checkpoint: Mutex<Option<Arc<RuntimeCheckpointCoordinator>>>,
    checkpoint_pending: Mutex<Vec<Arc<dyn crate::CheckpointParticipant>>>,
    fork: Option<Arc<dyn RuntimeForkPort>>,
    exec: Arc<ExecSlot>,
    memory: Mutex<Option<Arc<dyn ExitParticipant>>>,
    ipc: Mutex<Option<Arc<CheckpointIpcCatalog>>>,
    ipc_pipes: Mutex<Option<Arc<crate::IpcPipeRegistry>>>,
    locks: Arc<AdvisoryLockCoordinator>,
    system: Arc<crate::SystemAuthority>,
}

impl RuntimeAssembly {
    pub fn require(&self, domain: RuntimeDomain) -> Result<(), RuntimeAssemblyError> {
        match domain {
            RuntimeDomain::Task
            | RuntimeDomain::DescriptorEvent
            | RuntimeDomain::EventCatalog
            | RuntimeDomain::Network
            | RuntimeDomain::Provider
            | RuntimeDomain::Seccomp => Ok(()),
            RuntimeDomain::Checkpoint if self.checkpoint().is_some() => Ok(()),
            RuntimeDomain::Memory if self.memory().is_some() => Ok(()),
            RuntimeDomain::Ipc if self.ipc().is_some() => Ok(()),
            RuntimeDomain::Fork if self.fork.is_some() => Ok(()),
            RuntimeDomain::Execution if self.exec().is_some() => Ok(()),
            unsupported => Err(RuntimeAssemblyError::Unsupported(unsupported)),
        }
    }

    #[must_use]
    pub fn teardown(mut self) -> Vec<RuntimeDomain> {
        self.release()
    }

    #[must_use]
    pub fn tasks(&self) -> Arc<TaskRegistry> {
        self.tasks.as_ref().expect("constructed task registry").current()
    }

    pub fn system(&self) -> Arc<crate::SystemAuthority> {
        self.system.clone()
    }

    #[must_use]
    pub fn checkpoint_tasks(&self) -> Arc<CheckpointTaskRegistry> {
        self.tasks.as_ref().expect("constructed task registry").clone()
    }

    #[must_use]
    pub fn epoll(&self) -> Arc<Control> {
        self.epoll.as_ref().expect("constructed epoll control").clone()
    }

    #[must_use]
    pub fn descriptors(&self) -> Arc<RuntimeDescriptorTable> {
        self.descriptors.as_ref().expect("constructed descriptor table").clone()
    }

    #[must_use]
    pub fn checkpoint_descriptors(&self) -> Arc<crate::CheckpointDescriptorTable> {
        self.checkpoint_descriptors
            .as_ref()
            .expect("constructed checkpoint descriptor table")
            .clone()
    }

    pub fn install_memory(&self, memory: Arc<dyn ExitParticipant>) -> Result<(), RuntimeAssemblyError> {
        let mut current = self.memory.lock().unwrap_or_else(|error| error.into_inner());
        if current.is_some() {
            return Err(RuntimeAssemblyError::Construction(RuntimeDomain::Memory));
        }
        *current = Some(memory);
        Ok(())
    }

    #[must_use]
    pub fn memory(&self) -> Option<Arc<dyn ExitParticipant>> {
        self.memory.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub fn install_ipc(&self, objects: Arc<SharedObjectStore>) -> Result<(), RuntimeAssemblyError> {
        let shared_limits = SharedMemoryLimits::default();
        let shared = Arc::new(
            SharedMemoryNamespace::new(objects, shared_limits)
                .map_err(|_| RuntimeAssemblyError::Construction(RuntimeDomain::Ipc))?,
        );
        let message_limits = MessageLimits::default();
        let messages = Arc::new(
            MessageQueueNamespace::new(message_limits)
                .map_err(|_| RuntimeAssemblyError::Construction(RuntimeDomain::Ipc))?,
        );
        let semaphore_limits = SemaphoreLimits::default();
        let semaphores = Arc::new(
            SemaphoreNamespace::new(semaphore_limits)
                .map_err(|_| RuntimeAssemblyError::Construction(RuntimeDomain::Ipc))?,
        );
        let mut current = self.ipc.lock().unwrap_or_else(|error| error.into_inner());
        if current.is_some() {
            return Err(RuntimeAssemblyError::Construction(RuntimeDomain::Ipc));
        }
        let checkpoint = Arc::new(CheckpointIpcCatalog::new(Arc::new(IpcCatalog::new(
            shared,
            shared_limits,
            Vec::new(),
            messages,
            message_limits,
            semaphores,
            semaphore_limits,
            Vec::new(),
        ))));
        let bindings = Arc::new(crate::IpcPipeBindings::new());
        let registry = Arc::new(crate::IpcPipeRegistry::new(checkpoint.clone(), bindings));
        *self.ipc_pipes.lock().unwrap_or_else(|error| error.into_inner()) = Some(registry);
        *current = Some(checkpoint);
        Ok(())
    }

    #[must_use]
    pub fn ipc(&self) -> Option<Arc<IpcCatalog>> {
        self.ipc
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|catalog| catalog.current())
    }

    #[must_use]
    pub fn checkpoint_ipc(&self) -> Option<Arc<CheckpointIpcCatalog>> {
        self.ipc.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    #[must_use]
    pub fn ipc_pipes(&self) -> Option<Arc<crate::IpcPipeRegistry>> {
        self.ipc_pipes.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    #[must_use]
    pub fn locks(&self) -> Arc<AdvisoryLockCoordinator> {
        Arc::clone(&self.locks)
    }

    #[must_use]
    pub fn events(&self) -> Arc<EventCatalog> {
        self.events.as_ref().expect("constructed event catalog").current()
    }

    #[must_use]
    pub fn checkpoint_events(&self) -> Arc<crate::CheckpointEventCatalog> {
        self.events.as_ref().expect("constructed event catalog").clone()
    }

    #[must_use]
    pub fn event_resources(&self) -> Arc<crate::EventResourceRegistry> {
        self.event_resources
            .as_ref()
            .expect("constructed event resources")
            .clone()
    }

    #[must_use]
    pub fn event_bindings(&self) -> Arc<crate::EventObjectBindings> {
        self.event_bindings
            .as_ref()
            .expect("constructed event bindings")
            .clone()
    }

    #[must_use]
    pub fn network(&self) -> Arc<NetworkCatalog> {
        self.network.as_ref().expect("constructed network catalog").current()
    }

    #[must_use]
    pub fn checkpoint_network(&self) -> Arc<crate::CheckpointNetworkCatalog> {
        self.network.as_ref().expect("constructed network catalog").clone()
    }

    #[must_use]
    pub fn providers(&self) -> Arc<HandleNamespace> {
        self.providers
            .as_ref()
            .expect("constructed provider namespace")
            .current()
    }

    #[must_use]
    pub fn checkpoint_providers(&self) -> Arc<crate::CheckpointProviderNamespace> {
        self.providers.as_ref().expect("constructed provider namespace").clone()
    }

    #[must_use]
    pub fn provider_registry(&self) -> Arc<crate::ProviderRegistry> {
        self.provider_registry
            .as_ref()
            .expect("constructed provider registry")
            .clone()
    }

    #[must_use]
    pub fn seccomp(&self) -> Arc<SeccompControl> {
        self.seccomp.as_ref().expect("constructed seccomp control").clone()
    }

    pub fn install_fork(&mut self, fork: Arc<dyn RuntimeForkPort>) -> Result<(), RuntimeAssemblyError> {
        if self.fork.is_some() {
            return Err(RuntimeAssemblyError::Construction(RuntimeDomain::Fork));
        }
        self.fork = Some(fork);
        Ok(())
    }

    #[must_use]
    pub fn fork(&self) -> Option<Arc<dyn RuntimeForkPort>> {
        self.fork.clone()
    }

    pub fn install_exec(&self, exec: Arc<dyn RuntimeExecPort>) -> Result<(), RuntimeAssemblyError> {
        self.exec.install(exec)
    }

    #[must_use]
    pub fn exec(&self) -> Option<Arc<dyn RuntimeExecPort>> {
        self.exec.get()
    }

    #[must_use]
    pub fn exec_slot(&self) -> Arc<ExecSlot> {
        Arc::clone(&self.exec)
    }

    fn release(&mut self) -> Vec<RuntimeDomain> {
        let mut order = Vec::with_capacity(5);
        if self
            .checkpoint
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_some()
        {
            order.push(RuntimeDomain::Checkpoint);
        }
        self.checkpoint_pending
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        if self
            .exec
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_some()
        {
            order.push(RuntimeDomain::Execution);
        }
        if self.fork.take().is_some() {
            order.push(RuntimeDomain::Fork);
        }
        if self
            .memory
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_some()
        {
            order.push(RuntimeDomain::Memory);
        }
        self.ipc_pipes
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if self
            .ipc
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_some()
        {
            order.push(RuntimeDomain::Ipc);
        }
        if self.seccomp.take().is_some() {
            order.push(RuntimeDomain::Seccomp);
        }
        if self.network.take().is_some() {
            order.push(RuntimeDomain::Network);
        }
        self.provider_registry.take();
        if self.providers.take().is_some() {
            order.push(RuntimeDomain::Provider);
        }
        self.event_bindings.take();
        self.event_resources.take();
        if self.events.take().is_some() {
            order.push(RuntimeDomain::EventCatalog);
        }
        let checkpoint_descriptors = self.checkpoint_descriptors.take().is_some();
        let descriptors = self.descriptors.take().is_some();
        let epoll = self.epoll.take().is_some();
        if checkpoint_descriptors || descriptors || epoll {
            order.push(RuntimeDomain::DescriptorEvent);
        }
        if self.tasks.take().is_some() {
            order.push(RuntimeDomain::Task);
        }
        order
    }
}

impl Drop for RuntimeAssembly {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

impl From<TaskError> for RuntimeAssemblyError {
    fn from(_: TaskError) -> Self {
        Self::Construction(RuntimeDomain::Task)
    }
}

impl From<ControlError> for RuntimeAssemblyError {
    fn from(_: ControlError) -> Self {
        Self::Construction(RuntimeDomain::DescriptorEvent)
    }
}

impl From<EventCatalogError> for RuntimeAssemblyError {
    fn from(_: EventCatalogError) -> Self {
        Self::Construction(RuntimeDomain::EventCatalog)
    }
}

impl From<NamespaceError> for RuntimeAssemblyError {
    fn from(_: NamespaceError) -> Self {
        Self::Construction(RuntimeDomain::Provider)
    }
}

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
