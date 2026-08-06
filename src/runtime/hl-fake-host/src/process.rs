use crate::{FakeHost, FakeHostError, ResourceKind};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProcessToken(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessExit {
    Code(i32),
    Signal(i32),
}

pub struct ProcessAdapter {
    host: FakeHost,
    processes: Mutex<BTreeMap<ProcessToken, ProcessExit>>,
}

impl ProcessAdapter {
    #[must_use]
    pub fn new(host: FakeHost) -> Self {
        Self {
            host,
            processes: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn spawn(&self, exit: ProcessExit) -> Result<ProcessToken, FakeHostError> {
        let process = ProcessToken(self.host.allocate("process", ResourceKind::Process)?);
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(process, exit);
        Ok(process)
    }

    pub fn wait(&self, process: ProcessToken) -> Result<ProcessExit, FakeHostError> {
        let exit = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&process)
            .copied()
            .ok_or(FakeHostError::InvalidResource)?;
        self.host.record("process", "wait", process.0, 0, 0)?;
        Ok(exit)
    }

    pub fn terminate(&self, process: ProcessToken, signal: i32) -> Result<(), FakeHostError> {
        self.host.record("process", "terminate", process.0, 0, 0)?;
        *self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&process)
            .ok_or(FakeHostError::InvalidResource)? = ProcessExit::Signal(signal);
        Ok(())
    }

    pub fn close(&self, process: ProcessToken) -> Result<(), FakeHostError> {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&process)
            .ok_or(FakeHostError::InvalidResource)?;
        self.host.release("process", ResourceKind::Process, process.0)
    }
}
