use std::collections::BTreeSet;

use super::{TASK_CHECKPOINT_VERSION, TaskRegistryImage, TaskResourceKey};
use crate::{TaskError, TaskRegistry};

impl TaskRegistryImage {
    pub fn validate(&self) -> Result<(), TaskError> {
        if self.version != TASK_CHECKPOINT_VERSION {
            return Err(TaskError::InvalidSnapshot);
        }
        TaskRegistry::validate_snapshot(&self.registry)?;
        if !Self::ordered(&self.registry.processes, |value| value.id)
            || !Self::ordered(&self.registry.threads, |value| value.id)
            || !Self::ordered(&self.registry.sessions, |value| value.id)
            || !Self::ordered(&self.registry.process_groups, |value| value.id)
            || !Self::ordered(&self.registry.user_namespaces, |value| value.id)
            || !Self::ordered(&self.registry.uts_namespaces, |value| value.0)
            || !Self::ordered(&self.processes, |value| value.process)
            || !Self::ordered(&self.threads, |value| value.thread)
        {
            return Err(TaskError::InvalidSnapshot);
        }
        let expected_processes: BTreeSet<_> = self.registry.processes.iter().map(|value| value.id).collect();
        let expected_threads: BTreeSet<_> = self.registry.threads.iter().map(|value| value.id).collect();
        let mut processes = BTreeSet::new();
        for process in &self.processes {
            let descriptor_valid = process.descriptor_table.is_none_or(|key| key.0 != 0);
            if !processes.insert(process.process)
                || !descriptor_valid
                || !Self::distinct_keys(&process.shared_resources)
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        let mut threads = BTreeSet::new();
        for thread in &self.threads {
            let resources = [thread.execution, thread.tls, thread.host, thread.seccomp];
            if !threads.insert(thread.thread) || !Self::distinct_keys(&resources) {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        if processes != expected_processes || threads != expected_threads {
            return Err(TaskError::InvalidSnapshot);
        }
        Ok(())
    }

    fn distinct_keys(keys: &[TaskResourceKey]) -> bool {
        let mut found = BTreeSet::new();
        keys.iter().all(|key| key.0 != 0 && found.insert(*key))
    }

    fn ordered<T, K: Ord + Copy>(values: &[T], key: impl Fn(&T) -> K) -> bool {
        values.windows(2).all(|pair| key(&pair[0]) < key(&pair[1]))
    }
}
