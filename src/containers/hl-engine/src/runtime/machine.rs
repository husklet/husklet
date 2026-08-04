//! Concrete foundational Rust runtime factory with injected host/execution edges.

use crate::activation::GuestIsa;
use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory, RuntimeServices};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::launch_plan::RuntimeLaunchPlan;
use hl_runtime::{
    PortableTaskCodec, RuntimeAssembly, RuntimeAssemblyConfig, RuntimeDomain, RuntimeExecPort, RuntimeForkPort,
    TaskCheckpointParticipant,
};
use hl_task::{
    ProcessCheckpointReference, ProcessId, TaskError, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistryImage,
    TaskResourceKey, ThreadCheckpointReference, ThreadId,
};
use std::sync::Arc;

const RUNTIME_INIT_PROCESSES: usize = 1;
const UNTRUSTED_GUEST_PROCESSES: usize = 64;

/// Host capabilities required while constructing the available Rust domains.
pub trait HostServices: Send + Sync {
    fn exec_port(&self, assembly: &RuntimeAssembly) -> Result<Option<Arc<dyn RuntimeExecPort>>, CompositionError>;
    fn fork_port(&self, assembly: &RuntimeAssembly) -> Result<Option<Arc<dyn RuntimeForkPort>>, CompositionError>;
    fn validate(&self, assembly: &RuntimeAssembly) -> Result<(), CompositionError>;
}

pub use HostServices as RuntimeHostServices;

/// Execution remains an injected coarse boundary until native execution exists.
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

pub struct RustRuntimeFactory<E, H> {
    execution: Arc<E>,
    host: Arc<H>,
    defaults: RuntimeAssemblyConfig,
}

struct TaskBindings;

struct TaskRestore;

pub(crate) fn prepare_tasks(assembly: &RuntimeAssembly) -> Result<(), CompositionError> {
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
        .map_err(|_| CompositionError::RuntimeConstruction)
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
        if !image.processes.is_empty() || !image.threads.is_empty() {
            return Err(TaskError::InvalidSnapshot);
        }
        Ok(Box::new(TaskRestore))
    }
}

impl<E, H> RustRuntimeFactory<E, H> {
    #[must_use]
    pub fn new(execution: Arc<E>, host: Arc<H>, defaults: RuntimeAssemblyConfig) -> Self {
        Self {
            execution,
            host,
            defaults,
        }
    }

    fn assembly_config(
        &self,
        plan: &RuntimeLaunchPlan,
    ) -> Result<(RuntimeAssemblyConfig, hl_task::CpuTopology), CompositionError> {
        let mut config = self.defaults;
        // The isolated runtime exposes bounded sentry namespaces. Keep these
        // capacities in the composition policy rather than teaching syscall
        // implementations about individual workloads.
        if plan.options.get("HL_UNTRUSTED") == Some("1") {
            // TaskRegistry also owns the runtime's PID-1 parent. The sentry
            // capacity is guest-visible and excludes that internal parent.
            config.maximum_processes = config
                .maximum_processes
                .min(UNTRUSTED_GUEST_PROCESSES + RUNTIME_INIT_PROCESSES);
            config.descriptor_limit = config.descriptor_limit.min(1024);
        }
        let host_cpus = host_online_cpus();
        let online_cpus = plan
            .options
            .get("HL_CPUS")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|count| *count != 0)
            .unwrap_or(host_cpus)
            .clamp(1, hl_task::CpuTopology::MAXIMUM);
        if let Some(limit) = plan.options.get("HL_PIDS_MAX") {
            if let Ok(limit) = limit.parse::<usize>() {
                config.maximum_processes = limit;
            }
        }
        Ok((
            config,
            hl_task::CpuTopology::new(online_cpus).map_err(|_| CompositionError::RuntimeConstruction)?,
        ))
    }
}

fn host_online_cpus() -> usize {
    let sysfs = std::fs::read_to_string("/sys/devices/system/cpu/online")
        .ok()
        .and_then(|list| {
            list.trim().split(',').try_fold(0_usize, |total, range| {
                let (first, last) = range.split_once('-').map_or((range, range), |parts| parts);
                let first = first.parse::<usize>().ok()?;
                let last = last.parse::<usize>().ok()?;
                total.checked_add(last.checked_sub(first)?.checked_add(1)?)
            })
        });
    sysfs
        .filter(|count| *count != 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
}

pub struct RustRuntimeMachine<E> {
    isa: GuestIsa,
    plan: RuntimeLaunchPlan,
    assembly: RuntimeAssembly,
    execution: Arc<E>,
    services: RuntimeServices,
}

impl<E: GuestExecutionPort> RustRuntimeMachine<E> {
    pub fn checkpoint_supported(&self) -> Result<(), EngineError> {
        self.assembly
            .require(RuntimeDomain::Checkpoint)
            .map_err(|_| EngineError::Unsupported)
    }

    pub fn fork_supported(&self) -> Result<(), EngineError> {
        self.assembly
            .require(RuntimeDomain::Fork)
            .map_err(|_| EngineError::Unsupported)
    }

    pub fn exec_supported(&self) -> Result<(), EngineError> {
        self.assembly
            .require(RuntimeDomain::Execution)
            .map_err(|_| EngineError::Unsupported)
    }
}

impl<E: GuestExecutionPort> GuestMachine for RustRuntimeMachine<E> {
    fn start(&self) -> Result<(), EngineError> {
        self.execution
            .start(self.isa, &self.plan, &self.assembly, &self.services)
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        self.execution.wait(&self.assembly)
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        self.execution.stop(&self.assembly, request)
    }
}

impl<E, H> RuntimeFactory for RustRuntimeFactory<E, H>
where
    E: GuestExecutionPort,
    H: HostServices,
{
    type Machine = RustRuntimeMachine<E>;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        let (config, topology) = self.assembly_config(request.plan)?;
        let mut assembly =
            RuntimeAssembly::with_topology(config, topology).map_err(|_| CompositionError::RuntimeConstruction)?;
        if let Some(fork) = self.host.fork_port(&assembly)? {
            assembly
                .install_fork(fork)
                .map_err(|_| CompositionError::RuntimeConstruction)?;
        }
        if let Some(exec) = self.host.exec_port(&assembly)? {
            assembly
                .install_exec(exec)
                .map_err(|_| CompositionError::RuntimeConstruction)?;
        }
        prepare_tasks(&assembly)?;
        self.host.validate(&assembly)?;
        Ok(RustRuntimeMachine {
            isa: request.isa,
            plan: request.plan.clone(),
            assembly,
            execution: Arc::clone(&self.execution),
            services: request.services.clone(),
        })
    }
}

impl HostServices for hl_fake_host::FakeHost {
    fn exec_port(&self, _: &RuntimeAssembly) -> Result<Option<Arc<dyn RuntimeExecPort>>, CompositionError> {
        Ok(None)
    }

    fn fork_port(&self, _: &RuntimeAssembly) -> Result<Option<Arc<dyn RuntimeForkPort>>, CompositionError> {
        Ok(None)
    }

    fn validate(&self, assembly: &RuntimeAssembly) -> Result<(), CompositionError> {
        assembly
            .require(RuntimeDomain::Task)
            .map_err(|_| CompositionError::RuntimeConstruction)?;
        self.record("runtime", "validate", self.identity(), 0, 0)
            .map_err(|_| CompositionError::RuntimeConstruction)
    }
}

#[cfg(test)]
#[path = "machine_test.rs"]
mod tests;
