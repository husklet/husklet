use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_task::{
    ProcessCheckpointReference, ProcessId, TaskError, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistryImage,
    TaskResourceKey, ThreadCheckpointReference, ThreadId,
};

pub trait TaskBindingRestore: Send + Sync {
    fn stage(&self, image: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskBindingError {
    Capacity,
    Duplicate,
    Invalid,
}

struct BindingState {
    processes: BTreeMap<ProcessId, ProcessCheckpointReference>,
    threads: BTreeMap<ThreadId, ThreadCheckpointReference>,
}

pub struct TaskResourceCatalog {
    capacity: usize,
    state: Mutex<BindingState>,
    restore: Arc<dyn TaskBindingRestore>,
}

impl TaskResourceCatalog {
    pub fn new(capacity: usize, restore: Arc<dyn TaskBindingRestore>) -> Result<Self, TaskBindingError> {
        if capacity == 0 {
            return Err(TaskBindingError::Capacity);
        }
        Ok(Self {
            capacity,
            state: Mutex::new(BindingState {
                processes: BTreeMap::new(),
                threads: BTreeMap::new(),
            }),
            restore,
        })
    }

    pub fn bind_process(&self, reference: ProcessCheckpointReference) -> Result<(), TaskBindingError> {
        let mut state = self.state.lock().map_err(|_| TaskBindingError::Invalid)?;
        if !Self::valid_process(&reference) {
            return Err(TaskBindingError::Invalid);
        }
        if state.processes.len() == self.capacity {
            return Err(TaskBindingError::Capacity);
        }
        if state.processes.contains_key(&reference.process) {
            return Err(TaskBindingError::Duplicate);
        }
        state.processes.insert(reference.process, reference);
        Ok(())
    }

    pub fn bind_thread(&self, reference: ThreadCheckpointReference) -> Result<(), TaskBindingError> {
        let mut state = self.state.lock().map_err(|_| TaskBindingError::Invalid)?;
        if !Self::valid_thread(&reference) {
            return Err(TaskBindingError::Invalid);
        }
        if state.threads.len() == self.capacity {
            return Err(TaskBindingError::Capacity);
        }
        if state.threads.contains_key(&reference.thread) {
            return Err(TaskBindingError::Duplicate);
        }
        state.threads.insert(reference.thread, reference);
        Ok(())
    }

    pub fn unbind_process(&self, process: ProcessId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .processes
            .remove(&process);
    }

    pub fn unbind_thread(&self, thread: ThreadId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .threads
            .remove(&thread);
    }

    fn valid_process(reference: &ProcessCheckpointReference) -> bool {
        let mut keys = reference.shared_resources.clone();
        if let Some(descriptors) = reference.descriptor_table {
            keys.push(descriptors);
        }
        Self::distinct(&keys)
    }

    fn valid_thread(reference: &ThreadCheckpointReference) -> bool {
        Self::distinct(&[reference.execution, reference.tls, reference.host, reference.seccomp])
    }

    fn distinct(keys: &[TaskResourceKey]) -> bool {
        let mut keys = keys.to_vec();
        keys.sort_unstable();
        keys.iter().all(|key| key.0 != 0) && keys.windows(2).all(|pair| pair[0] != pair[1])
    }
}

impl TaskExternalCheckpoint for TaskResourceCatalog {
    fn snapshot_process(&self, process: ProcessId) -> Result<ProcessCheckpointReference, TaskError> {
        self.state
            .lock()
            .map_err(|_| TaskError::InvalidSnapshot)?
            .processes
            .get(&process)
            .cloned()
            .ok_or(TaskError::InvalidSnapshot)
    }

    fn snapshot_thread(&self, thread: ThreadId) -> Result<ThreadCheckpointReference, TaskError> {
        self.state
            .lock()
            .map_err(|_| TaskError::InvalidSnapshot)?
            .threads
            .get(&thread)
            .cloned()
            .ok_or(TaskError::InvalidSnapshot)
    }

    fn stage(&self, image: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError> {
        image.validate()?;
        self.restore.stage(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

    struct Restore;
    struct Transaction;
    impl TaskExternalRestore for Transaction {
        fn commit(&mut self) -> Result<(), TaskError> {
            Ok(())
        }
        fn rollback(&mut self) {}
        fn resume(&mut self) -> Result<(), TaskError> {
            Ok(())
        }
    }
    impl TaskBindingRestore for Restore {
        fn stage(&self, _: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError> {
            Ok(Box::new(Transaction))
        }
    }

    #[test]
    fn catalog_keys_bound() {
        let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (process, thread) = registry
            .create_init(ProcessCredentials::new(0, 0, &[], 4).unwrap(), ProcessLimits::default())
            .unwrap();
        let catalog = TaskResourceCatalog::new(2, Arc::new(Restore)).unwrap();
        catalog
            .bind_process(ProcessCheckpointReference {
                process,
                descriptor_table: Some(TaskResourceKey(1)),
                shared_resources: vec![TaskResourceKey(2)],
            })
            .unwrap();
        catalog
            .bind_thread(ThreadCheckpointReference {
                thread,
                execution: TaskResourceKey(3),
                tls: TaskResourceKey(4),
                host: TaskResourceKey(5),
                seccomp: TaskResourceKey(6),
            })
            .unwrap();
        registry.freeze_checkpoint();
        let image = registry.image(&catalog).unwrap();
        registry.thaw_checkpoint();
        assert_eq!(image.threads[0].seccomp, TaskResourceKey(6));
        assert!(catalog.stage(&image).is_ok());
    }

    #[test]
    fn invalid_keys_rejected() {
        let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (process, thread) = registry
            .create_init(ProcessCredentials::new(0, 0, &[], 4).unwrap(), ProcessLimits::default())
            .unwrap();
        let catalog = TaskResourceCatalog::new(1, Arc::new(Restore)).unwrap();
        assert_eq!(
            catalog.bind_process(ProcessCheckpointReference {
                process,
                descriptor_table: Some(TaskResourceKey(1)),
                shared_resources: vec![TaskResourceKey(1)],
            }),
            Err(TaskBindingError::Invalid)
        );
        assert_eq!(
            catalog.bind_thread(ThreadCheckpointReference {
                thread,
                execution: TaskResourceKey(1),
                tls: TaskResourceKey(2),
                host: TaskResourceKey(3),
                seccomp: TaskResourceKey(3),
            }),
            Err(TaskBindingError::Invalid)
        );
    }
}
