use hl_linux::GuestMemory;
use hl_runtime::{RuntimeAssembly, RuntimeEventSyscalls};

#[derive(Clone)]
pub(super) struct Resources {
    bindings: std::sync::Arc<hl_runtime::EventObjectBindings>,
    resources: std::sync::Arc<hl_runtime::EventResourceRegistry>,
}

impl Resources {
    pub(super) fn new(assembly: &RuntimeAssembly) -> Self {
        Self {
            bindings: assembly.event_bindings(),
            resources: assembly.event_resources(),
        }
    }

    pub(super) fn configure<M: GuestMemory>(&self, runtime: RuntimeEventSyscalls<M>) -> RuntimeEventSyscalls<M> {
        runtime.with_checkpoint_resources(self.bindings.clone(), self.resources.clone())
    }
}
