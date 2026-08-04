use std::sync::Arc;

pub(super) struct MemoryLimit {
    tasks: Arc<hl_task::TaskRegistry>,
    process: hl_task::ProcessId,
}

impl MemoryLimit {
    pub(super) fn new(tasks: Arc<hl_task::TaskRegistry>, process: hl_task::ProcessId) -> Self {
        Self { tasks, process }
    }
}

impl std::fmt::Debug for MemoryLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryLimit")
            .field("process", &self.process)
            .finish()
    }
}

impl super::super::memory_control::LimitSource for MemoryLimit {
    fn soft(&self) -> u64 {
        self.tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|process| process.id == self.process)
            .and_then(|process| {
                process
                    .limits
                    .into_iter()
                    .find_map(|(resource, limit)| (resource == hl_task::Resource::LockedMemory).then_some(limit.soft))
            })
            .unwrap_or(0)
    }
}
