use std::sync::Arc;

use hl_memory::{AtomicBatchHost, ExitMappingHost, MappingHost};

use crate::{
    DescriptorExit, ExitRuntime, IpcExitHandler, MemoryExit, RegistryExitFinalizer, RobustExitHandler, VfsLockExit,
};

/// Complete production dependencies for process-exit composition.
///
/// Construction requires every reversible domain plus terminal task
/// finalization. Private fields prevent partial struct literals, and `build`
/// preserves the runtime's fixed publication order.
///
/// Omitting a domain is a type error:
///
/// ```compile_fail
/// use std::sync::Arc;
/// use hl_memory::{AtomicBatchHost, ExitMappingHost, MappingHost};
/// use hl_runtime::{
///     DescriptorExit, ExitRuntimeDependencies, IpcExitHandler, MemoryExit,
///     RobustExitHandler, VfsLockExit,
/// };
///
/// fn missing_task<R, I, M>(
///     robust: Arc<RobustExitHandler<R>>,
///     descriptors: Arc<DescriptorExit>,
///     ipc: Arc<IpcExitHandler<I>>,
///     memory: Arc<MemoryExit<M>>,
///     locks: Arc<VfsLockExit>,
/// ) where
///     R: AtomicBatchHost + 'static,
///     I: MappingHost + 'static,
///     M: ExitMappingHost + 'static,
///     M::PreparedExit: 'static,
/// {
///     let _ = ExitRuntimeDependencies::new(
///         robust, descriptors, ipc, memory, locks,
///     );
/// }
/// ```
pub struct ExitRuntimeDependencies<R, I, M>
where
    R: AtomicBatchHost,
    I: MappingHost,
    M: ExitMappingHost,
{
    robust: Arc<RobustExitHandler<R>>,
    descriptors: Arc<DescriptorExit>,
    ipc: Arc<IpcExitHandler<I>>,
    memory: Arc<MemoryExit<M>>,
    locks: Arc<VfsLockExit>,
    task: Arc<RegistryExitFinalizer>,
}

impl<R, I, M> ExitRuntimeDependencies<R, I, M>
where
    R: AtomicBatchHost + 'static,
    I: MappingHost + 'static,
    M: ExitMappingHost + 'static,
    M::PreparedExit: 'static,
{
    /// Captures every mandatory process-exit domain.
    #[must_use]
    pub fn new(
        robust: Arc<RobustExitHandler<R>>,
        descriptors: Arc<DescriptorExit>,
        ipc: Arc<IpcExitHandler<I>>,
        memory: Arc<MemoryExit<M>>,
        locks: Arc<VfsLockExit>,
        task: Arc<RegistryExitFinalizer>,
    ) -> Self {
        Self {
            robust,
            descriptors,
            ipc,
            memory,
            locks,
            task,
        }
    }

    /// Builds the fixed-order production exit coordinator.
    #[must_use]
    pub fn build(self) -> ExitRuntime {
        ExitRuntime::new(
            self.robust,
            self.descriptors,
            self.ipc,
            self.memory,
            self.locks,
            self.task,
        )
    }
}
