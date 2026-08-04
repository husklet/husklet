use super::HostError;
use std::ffi::CString;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessId(u32);

impl ProcessId {
    pub fn new(value: u32) -> Result<Self, HostError> {
        (value != 0).then_some(Self(value)).ok_or(HostError::Invalid)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessGroup {
    Inherit,
    New,
    Join(ProcessId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessSignal {
    Terminate,
    Kill,
    Interrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildExit {
    Code(u8),
    Signal(u8),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostDescriptor(i32);

impl HostDescriptor {
    pub fn new(value: i32) -> Result<Self, HostError> {
        (value >= 0).then_some(Self(value)).ok_or(HostError::Invalid)
    }

    pub(crate) const fn raw(self) -> i32 {
        self.0
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileAction {
    Duplicate {
        source: HostDescriptor,
        target: HostDescriptor,
    },
    Close(HostDescriptor),
    Inherit(HostDescriptor),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnRequest {
    pub program: CString,
    pub arguments: Vec<CString>,
    pub environment: Vec<CString>,
    pub process_group: ProcessGroup,
    pub file_actions: Vec<FileAction>,
}

impl SpawnRequest {
    pub fn validate(&self) -> Result<(), HostError> {
        if self.file_actions.len() > 64 {
            return Err(HostError::Exhausted);
        }
        let mut targets = std::collections::BTreeSet::new();
        for action in &self.file_actions {
            let target = match action {
                FileAction::Duplicate { target, .. } | FileAction::Inherit(target) => *target,
                FileAction::Close(_) => continue,
            };
            if !targets.insert(target) {
                return Err(HostError::Invalid);
            }
        }
        Ok(())
    }
}

pub trait ProcessSyscalls: Send + Sync {
    fn spawn(&self, request: &SpawnRequest) -> Result<(ProcessId, u64), HostError>;
    fn close_process(&self, token: u64);
    fn wait(&self, process: ProcessId) -> Result<Option<ChildExit>, HostError>;
    fn wait_blocking(&self, process: ProcessId) -> Result<ChildExit, HostError>;
    fn signal(&self, process: ProcessId, signal: ProcessSignal) -> Result<(), HostError>;
    fn signal_group(&self, group: ProcessId, signal: ProcessSignal) -> Result<(), HostError>;
}

pub struct ProcessHandle<S: ProcessSyscalls> {
    syscalls: Arc<S>,
    process: ProcessId,
    token: Option<u64>,
}

impl<S: ProcessSyscalls> ProcessHandle<S> {
    pub fn spawn(syscalls: Arc<S>, request: &SpawnRequest) -> Result<Self, HostError> {
        let (process, token) = syscalls.spawn(request)?;
        Ok(Self {
            syscalls,
            process,
            token: Some(token),
        })
    }

    #[must_use]
    pub const fn id(&self) -> ProcessId {
        self.process
    }

    pub fn wait(&self) -> Result<Option<ChildExit>, HostError> {
        self.syscalls.wait(self.process)
    }

    pub fn wait_blocking(&self) -> Result<ChildExit, HostError> {
        self.syscalls.wait_blocking(self.process)
    }

    pub fn signal(&self, signal: ProcessSignal) -> Result<(), HostError> {
        self.syscalls.signal(self.process, signal)
    }

    pub fn signal_group(&self, signal: ProcessSignal) -> Result<(), HostError> {
        self.syscalls.signal_group(self.process, signal)
    }
}

impl<S: ProcessSyscalls> Drop for ProcessHandle<S> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.syscalls.close_process(token);
        }
    }
}
