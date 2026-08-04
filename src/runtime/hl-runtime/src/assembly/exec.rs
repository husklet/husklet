use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_task::ProcessId;

use crate::RuntimeExecPort;

use super::{RuntimeAssemblyError, RuntimeDomain};

pub struct ExecSlot {
    pub(super) current: Mutex<Option<Arc<dyn RuntimeExecPort>>>,
    processes: Mutex<BTreeMap<ProcessId, Arc<dyn RuntimeExecPort>>>,
}

impl ExecSlot {
    pub(super) fn new() -> Self {
        Self {
            current: Mutex::new(None),
            processes: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn install(&self, exec: Arc<dyn RuntimeExecPort>) -> Result<(), RuntimeAssemblyError> {
        let mut current = self.current.lock().unwrap_or_else(|error| error.into_inner());
        if current.is_some() {
            return Err(RuntimeAssemblyError::Construction(RuntimeDomain::Execution));
        }
        *current = Some(exec);
        Ok(())
    }

    #[must_use]
    pub fn get(&self) -> Option<Arc<dyn RuntimeExecPort>> {
        self.current.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub fn register(&self, process: ProcessId, exec: Arc<dyn RuntimeExecPort>) -> Result<(), RuntimeAssemblyError> {
        let mut processes = self.processes.lock().unwrap_or_else(|error| error.into_inner());
        if processes.contains_key(&process) {
            return Err(RuntimeAssemblyError::Construction(RuntimeDomain::Execution));
        }
        processes.insert(process, exec);
        Ok(())
    }

    #[must_use]
    pub fn for_process(&self, process: ProcessId) -> Option<Arc<dyn RuntimeExecPort>> {
        self.processes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&process)
            .cloned()
            .or_else(|| self.get())
    }

    pub fn unregister(&self, process: ProcessId) {
        self.processes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&process);
    }
}
