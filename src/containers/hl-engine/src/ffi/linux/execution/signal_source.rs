use std::sync::Arc;

use hl_runtime::{EventResourceKey, SignalEventSource, SourceError, TaskSignalQueue};

#[derive(Debug)]
pub(super) struct Source {
    queue: Arc<TaskSignalQueue>,
    resource: EventResourceKey,
}

impl Source {
    pub(super) fn new(tasks: Arc<hl_task::TaskRegistry>, thread: hl_task::ThreadId) -> Self {
        Self {
            queue: Arc::new(TaskSignalQueue::new(tasks, thread)),
            resource: EventResourceKey::new(0x2000_0000_0000_0000 | u64::from(thread.number()))
                .expect("thread numbers are nonzero"),
        }
    }
}

impl SignalEventSource for Source {
    fn queue(&self) -> Result<(EventResourceKey, Arc<dyn hl_event::SignalQueue>), SourceError> {
        Ok((self.resource, self.queue.clone()))
    }
}
