use std::sync::Weak;

use hl_task::ThreadId;

pub(super) struct Wake {
    threads: Weak<super::super::threads::ThreadSet>,
    thread: ThreadId,
}

impl Wake {
    pub(super) const fn new(threads: Weak<super::super::threads::ThreadSet>, thread: ThreadId) -> Self {
        Self { threads, thread }
    }
}

impl hl_runtime::TraceWake for Wake {
    fn wake(&self) {
        if let Some(threads) = self.threads.upgrade() {
            let _ = threads.resume(self.thread);
        }
    }
}
