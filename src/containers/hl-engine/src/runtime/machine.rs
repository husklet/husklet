//! Temporary execution-port seam shared by the retained C adapter and the
//! test-only Rust executor closure.

use crate::activation::GuestIsa;
use crate::composition::RuntimeServices;
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::launch_plan::RuntimeLaunchPlan;
use hl_runtime::RuntimeAssembly;
use hl_runtime::{PortableTaskCodec, TaskCheckpointParticipant};
use hl_task::{
    ProcessCheckpointReference, ProcessId, TaskError, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistryImage,
    TaskResourceKey, ThreadCheckpointReference, ThreadId,
};
use std::sync::Arc;

pub trait GuestExecutionPort: Send + Sync {
    fn start(
        &self,
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        assembly: &RuntimeAssembly,
        services: &RuntimeServices,
    ) -> Result<(), EngineError>;
    fn wait(&self, assembly: &RuntimeAssembly) -> Result<EngineExit, EngineError>;
    fn stop(&self, assembly: &RuntimeAssembly, request: StopRequest) -> Result<(), EngineError>;
}

struct TaskBindings;
struct TaskRestore;

pub(crate) fn prepare_tasks(assembly: &RuntimeAssembly) -> Result<(), crate::composition::CompositionError> {
    if assembly.has_checkpoint_role(hl_runtime::CheckpointRole::Task) {
        return Ok(());
    }
    assembly
        .prepare_checkpoint(Arc::new(
            TaskCheckpointParticipant::new(
                assembly.checkpoint_tasks(),
                Arc::new(TaskBindings),
                Arc::new(PortableTaskCodec),
            )
            .with_seccomp(assembly.seccomp()),
        ))
        .map_err(|_| crate::composition::CompositionError::RuntimeConstruction)
}

impl TaskExternalRestore for TaskRestore {
    fn commit(&mut self) -> Result<(), TaskError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn resume(&mut self) -> Result<(), TaskError> {
        Ok(())
    }
}

impl TaskExternalCheckpoint for TaskBindings {
    fn snapshot_process(&self, process: ProcessId) -> Result<ProcessCheckpointReference, TaskError> {
        Ok(ProcessCheckpointReference {
            process,
            descriptor_table: None,
            shared_resources: Vec::new(),
        })
    }

    fn snapshot_thread(&self, thread: ThreadId) -> Result<ThreadCheckpointReference, TaskError> {
        let key = u64::from(thread.number()) + 1;
        Ok(ThreadCheckpointReference {
            thread,
            execution: TaskResourceKey(key),
            tls: TaskResourceKey(key),
            host: TaskResourceKey(key),
            seccomp: TaskResourceKey(key),
        })
    }

    fn stage(&self, image: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError> {
        if image.processes.len() != 1
            || image.threads.len() != 1
            || image.registry.processes.len() != 1
            || image.registry.threads.len() != 1
        {
            return Err(TaskError::InvalidSnapshot);
        }
        Ok(Box::new(TaskRestore))
    }
}
