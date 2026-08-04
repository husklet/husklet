//! Thread-clone composition owned by the clone domain.

use std::sync::{Arc, OnceLock, Weak};

use hl_runtime::{RuntimeThreadPort, ThreadCloneError, ThreadCloneRuntime, ThreadCloneTrap, ThreadContextPort};
use hl_task::ThreadId;

use super::process_memory::ProcessMemory;
use super::{readiness, routing, threads};

pub(super) struct Contexts {
    process: Arc<routing::ProcessContext>,
    threads: Arc<threads::ThreadSet>,
    runtime: OnceLock<Weak<ThreadCloneRuntime<ProcessMemory>>>,
}

impl Contexts {
    pub(super) fn new(process: Arc<routing::ProcessContext>, threads: Arc<threads::ThreadSet>) -> Self {
        Self {
            process,
            threads,
            runtime: OnceLock::new(),
        }
    }

    pub(super) fn install(&self, runtime: Arc<ThreadCloneRuntime<ProcessMemory>>) -> Result<(), ()> {
        self.runtime.set(Arc::downgrade(&runtime)).map_err(|_| ())
    }

    pub(super) fn runtime(&self) -> Option<Arc<ThreadCloneRuntime<ProcessMemory>>> {
        self.runtime.get().and_then(Weak::upgrade)
    }

    pub(super) fn build(self: &Arc<Self>) -> Arc<ThreadCloneRuntime<ProcessMemory>> {
        let runnable: Arc<dyn RuntimeThreadPort> = self.threads.clone();
        let context: Arc<dyn ThreadContextPort> = self.clone();
        Arc::new(ThreadCloneRuntime::new(
            self.process.tasks(),
            runnable,
            context,
            self.process.memory(),
        ))
    }
}

impl ThreadContextPort for Contexts {
    fn prepare(&self, source: ThreadId, thread: ThreadId) -> Result<(), ThreadCloneError> {
        let runtime = self.runtime().ok_or(ThreadCloneError::Failed)?;
        let cancellation = Arc::new(readiness::Cancellation::new().map_err(|_| ThreadCloneError::NoMemory)?);
        let trap = ThreadCloneTrap::new(runtime, thread);
        let files = self.process.inherit_files(source, thread);
        let router = self
            .process
            .router(thread, Arc::clone(&cancellation), Some(Box::new(trap)));
        drop(files);
        self.threads
            .prepare(
                thread,
                self.process.process_id(),
                Arc::new(router),
                cancellation,
                self.process.space(),
            )
            .map_err(|error| match error {
                hl_runtime::RuntimeThreadError::Capacity => ThreadCloneError::Again,
                hl_runtime::RuntimeThreadError::Invalid => ThreadCloneError::Invalid,
                _ => ThreadCloneError::Failed,
            })
    }

    fn rollback(&self, thread: ThreadId) {
        self.threads.discard(thread);
        self.process.forget_files(thread);
    }
}
