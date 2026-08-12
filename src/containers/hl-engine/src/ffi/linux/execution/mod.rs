use super::{BackingChanges, MappingHostAdapter, VirtualMemory};
use crate::activation::GuestIsa;
#[cfg(test)]
use crate::engine::StopRequest;
use crate::engine::{EngineError, EngineExit, ExitKind};
use crate::launch_plan::RuntimeLaunchPlan;
#[cfg(test)]
use crate::runtime_machine::GuestExecutionPort;
#[cfg(test)]
use hl_execution::{Aarch64CpuState, CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionSnapshot};
use hl_isa::GuestArchitecture;
use hl_loader::GuestFeatures;
#[cfg(test)]
use hl_memory::MappingCoordinator;
#[cfg(test)]
use hl_runtime::RuntimeSyscallRouter;
#[cfg(test)]
use hl_runtime::{RuntimeAssembly, RuntimeThreadPort};
use std::sync::{Arc, Mutex};

mod arena;
mod atomic_memory;
mod checkpoint;
mod clone;
mod descriptor;
mod diagnostics;
mod exec_image;
mod exec_retire;
mod exec_transaction;
mod exit;
mod fork;
mod image_data;
mod itimer;
mod launch;
mod memory_account;
mod memory_limit;
pub(crate) use crate::ffi::linux::network;
mod operand;
mod outcome;
pub(in crate::ffi::linux) mod path;
mod ports;
pub(in crate::ffi::linux) mod process_memory;
mod process_resources;
mod readiness;
mod routing;
mod scheduler;
mod service;
mod signal_frame;
mod signal_source;
mod source;
mod space;
mod state;
mod syscall;
mod task;
mod threads;
#[cfg(test)]
#[path = "threads_test.rs"]
mod threads_test;
mod transfer;
mod vector;
mod waiter;
mod watch;
#[cfg(test)]
const ARENA_LENGTH: usize = arena::Capacity::DEFAULT;

use operand::ArenaMemory;

use state::State;

use scheduler::ThreadTerminal;

pub struct GuestExecutor {
    state: Mutex<State>,
    resources: Arc<crate::native_host::HostResourceContext>,
    entropy_source: Arc<image_data::Entropy>,
    authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
    entropy: Option<[u8; 16]>,
    host_faults: Option<Arc<dyn crate::native::HostFaultOwner>>,
}
impl Default for GuestExecutor {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            resources: crate::native_host::HostResourceContext::new(),
            entropy_source: Arc::new(image_data::Entropy),
            authority: None,
            entropy: None,
            host_faults: None,
        }
    }
}
impl GuestExecutor {
    fn guest_features(architecture: GuestArchitecture) -> GuestFeatures {
        match architecture {
            GuestArchitecture::Aarch64 => GuestFeatures {
                hardware: 0x10_01fb,
                hardware_second: 0,
            },
            GuestArchitecture::X86_64 => GuestFeatures {
                hardware: u64::from(hl_execution::GuestFeaturePolicy::interpreter().cpuid(1, 0).edx),
                hardware_second: 0,
            },
        }
    }

    pub fn prepare_entropy() -> Result<[u8; 16], EngineError> {
        image_data::Entropy.read().map_err(|_| EngineError::LaunchFailed)
    }

    pub fn authorized(authority: Arc<Mutex<crate::native::AuthorityWorker>>, entropy: [u8; 16]) -> Self {
        Self {
            state: Mutex::new(State::default()),
            resources: crate::native_host::HostResourceContext::new(),
            entropy_source: Arc::new(image_data::Entropy),
            authority: Some(authority),
            entropy: Some(entropy),
            host_faults: None,
        }
    }
    fn cancelled(plan: &RuntimeLaunchPlan, threads: &threads::ThreadSet) -> Result<Option<EngineExit>, EngineError> {
        let Some((router, signal)) = threads.signal() else {
            return Ok(None);
        };
        threads.terminate_all();
        diagnostics::TraceReport::new(plan, &router).write()?;
        Ok(Some(Self::signal(signal)))
    }

    fn finish(
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        run: threads::ThreadRun,
        terminal: ThreadTerminal,
    ) -> Result<Option<EngineExit>, EngineError> {
        let exit = match terminal {
            ThreadTerminal::Thread(exit) => {
                threads.terminate_run(&run).map_err(Self::thread_error)?;
                exit
            }
            ThreadTerminal::Group(exit) => {
                threads.terminate_group(&run).map_err(Self::thread_error)?;
                exit
            }
        };
        threads.note_process_exit(run.process, exit);
        if !threads.is_empty() {
            return Ok(None);
        }
        diagnostics::TraceReport::new(plan, &run.router).write()?;
        Ok(Some(threads.session_exit(exit)))
    }

    #[cfg(test)]
    fn router(
        arena: Arc<VirtualMemory>,
        mappings: Arc<MappingCoordinator<MappingHostAdapter>>,
        plan: &RuntimeLaunchPlan,
        assembly: &RuntimeAssembly,
        architecture: hl_linux::GuestArchitecture,
        cancellation: Arc<readiness::Cancellation>,
    ) -> Result<RuntimeSyscallRouter, EngineError> {
        if assembly.ipc().is_none() {
            let shared = Arc::new(
                hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default())
                    .map_err(|_| EngineError::LaunchFailed)?,
            );
            assembly.install_ipc(shared).map_err(|_| EngineError::LaunchFailed)?;
        }
        routing::create(
            arena,
            mappings,
            plan,
            assembly,
            architecture,
            cancellation,
            None,
            Arc::new(image_data::Entropy),
            &crate::composition::StandardStreams::default(),
        )
        .map(|routed| routed.router)
    }

    /// Syscalls not listed here currently return `ENOSYS` through the router.
    #[must_use]
    pub const fn supported_syscalls() -> &'static [&'static str] {
        ports::SUPPORTED
    }

    fn fault(
        isa: GuestIsa,
        memory: &impl hl_execution::ExecutionInstructionMemory,
        fault: hl_execution::ExecutionFault,
    ) -> EngineExit {
        let (detail, reason, operand) = match fault {
            hl_execution::ExecutionFault::Fetch(value) => (
                value.instruction,
                crate::engine::FaultReason::Fetch,
                Some((value.address, value.access)),
            ),
            hl_execution::ExecutionFault::Memory(value) => (
                value.instruction,
                crate::engine::FaultReason::Memory,
                Some((value.address, value.access)),
            ),
            hl_execution::ExecutionFault::Operand(value) => {
                let fault = value.fault();
                (
                    fault.instruction,
                    crate::engine::FaultReason::Memory,
                    Some((fault.address, fault.access)),
                )
            }
            hl_execution::ExecutionFault::Alignment {
                instruction,
                address,
                access,
            } => (instruction, crate::engine::FaultReason::Memory, Some((address, access))),
            hl_execution::ExecutionFault::Decode { instruction } => {
                (instruction, crate::engine::FaultReason::Decode, None)
            }
            hl_execution::ExecutionFault::Unsupported { instruction } => {
                (instruction, crate::engine::FaultReason::Unsupported, None)
            }
            hl_execution::ExecutionFault::Signal(signal) => {
                (signal.instruction, crate::engine::FaultReason::Protocol, None)
            }
            hl_execution::ExecutionFault::Frozen => (0, crate::engine::FaultReason::Frozen, None),
            hl_execution::ExecutionFault::CacheEpoch => (0, crate::engine::FaultReason::CacheEpoch, None),
            hl_execution::ExecutionFault::Protocol => (0, crate::engine::FaultReason::Protocol, None),
            hl_execution::ExecutionFault::NativeFatal { code } => (code, crate::engine::FaultReason::NativeFatal, None),
        };
        let mut opcode = [0_u8; 15];
        // `detail` is an invariant code rather than a program counter for a native fatal.
        let opcode_len = if detail == 0 || reason == crate::engine::FaultReason::NativeFatal {
            0
        } else {
            memory.fetch(detail, &mut opcode).map_or(0, |length| length as u8)
        };
        EngineExit {
            kind: ExitKind::Fault,
            guest_status: 0,
            detail,
            fault: Some(crate::engine::FaultDiagnostic {
                isa,
                pc: detail,
                opcode,
                opcode_len,
                reason,
                address: operand.map(|value| value.0),
                access: operand.map(|value| match value.1 {
                    hl_execution::AccessKind::Read => crate::engine::FaultAccess::Read,
                    hl_execution::AccessKind::Write => crate::engine::FaultAccess::Write,
                    hl_execution::AccessKind::Execute => crate::engine::FaultAccess::Execute,
                }),
            }),
        }
    }
}

#[cfg(test)]
mod test;
