use std::sync::Arc;

use super::super::{fork, process_memory::ProcessMemory, threads};

impl super::ProcessContext {
    pub(in crate::ffi::linux::execution) fn network_bindings(
        &self,
    ) -> Arc<hl_runtime::NetworkObjectBindings<super::super::network::Native>> {
        self.network.bindings()
    }

    pub(in crate::ffi::linux::execution) fn memory(&self) -> ProcessMemory {
        self.space.guest_memory()
    }

    pub(in crate::ffi::linux::execution) fn prepare_exec_retire(
        &self,
        caller: hl_task::ThreadId,
    ) -> Result<super::super::exec_retire::RetireImage, hl_runtime::RuntimeExecError> {
        super::super::exec_retire::RetireImage::prepare(
            &self.tasks,
            self.process,
            caller,
            self.space.mappings(),
            self.futex.clone(),
        )
    }

    pub(in crate::ffi::linux::execution) fn install_fork(&self, fork: &Arc<fork::Runtime>) -> Result<(), ()> {
        self.fork.set(Arc::downgrade(fork)).map_err(|_| ())
    }

    pub(in crate::ffi::linux::execution) fn install_threads(
        &self,
        threads: &Arc<threads::ThreadSet>,
    ) -> Result<(), ()> {
        self.threads.set(Arc::downgrade(threads)).map_err(|_| ())
    }

    pub(in crate::ffi::linux::execution) fn install_clone(
        &self,
        context: &Arc<super::super::clone::Contexts>,
    ) -> Result<(), ()> {
        self.clone_context.set(Arc::downgrade(context)).map_err(|_| ())
    }
}
