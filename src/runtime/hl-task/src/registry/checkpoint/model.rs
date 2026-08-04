use crate::{ProcessId, TaskError, ThreadId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TaskResourceKey(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessCheckpointReference {
    pub process: ProcessId,
    pub descriptor_table: Option<TaskResourceKey>,
    pub shared_resources: Vec<TaskResourceKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadCheckpointReference {
    pub thread: ThreadId,
    pub execution: TaskResourceKey,
    pub tls: TaskResourceKey,
    pub host: TaskResourceKey,
    pub seccomp: TaskResourceKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskRegistryImage {
    pub version: u32,
    pub registry: crate::RegistrySnapshot,
    pub processes: Vec<ProcessCheckpointReference>,
    pub threads: Vec<ThreadCheckpointReference>,
}

pub trait TaskExternalRestore: Send {
    fn commit(&mut self) -> Result<(), TaskError>;
    fn rollback(&mut self);
    fn resume(&mut self) -> Result<(), TaskError>;
}

pub trait TaskExternalCheckpoint: Send + Sync {
    fn snapshot_process(&self, process: ProcessId) -> Result<ProcessCheckpointReference, TaskError>;

    fn snapshot_thread(&self, thread: ThreadId) -> Result<ThreadCheckpointReference, TaskError>;

    fn stage(&self, image: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError>;
}
