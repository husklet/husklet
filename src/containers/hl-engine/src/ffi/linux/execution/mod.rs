use super::{BackingChanges, MappingHostAdapter, VirtualMemory};
use crate::activation::GuestIsa;
use crate::composition::RuntimeServices;
#[cfg(test)]
use crate::engine::StopRequest;
use crate::engine::{EngineError, EngineExit, ExitKind};
use crate::launch_plan::RuntimeLaunchPlan;
#[cfg(test)]
use crate::runtime_machine::GuestExecutionPort;
use hl_execution::{Aarch64CpuState, CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionSnapshot};
use hl_isa::GuestArchitecture;
use hl_loader::{GuestCredentials, GuestFeatures, LoadLimits, LoadRequest, Loader};
use hl_memory::MappingCoordinator;
#[cfg(test)]
use hl_runtime::RuntimeSyscallRouter;
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
mod exit;
mod fork;
mod image_data;
mod itimer;
mod memory_account;
mod memory_limit;
pub(crate) mod network;
mod operand;
mod outcome;
mod path;
mod ports;
mod process_memory;
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
    fn run(
        &self,
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        assembly: &RuntimeAssembly,
        services: &RuntimeServices,
        cancellation: Arc<readiness::Cancellation>,
        threads: Arc<threads::ThreadSet>,
    ) -> Result<EngineExit, EngineError> {
        crate::runtime_machine::prepare_tasks(assembly).map_err(|_| EngineError::LaunchFailed)?;
        let guest_executable = routing::image::WorkspaceRoot::executable(plan).ok_or(EngineError::LaunchFailed)?;
        let path = if plan.rootfs.is_some() {
            guest_executable.as_slice()
        } else {
            plan.executable_host
                .as_deref()
                .or_else(|| plan.arguments.first().map(Vec::as_slice))
                .ok_or(EngineError::LaunchFailed)?
        };
        let architecture = match isa {
            GuestIsa::Aarch64 => GuestArchitecture::Aarch64,
            GuestIsa::X86_64 => GuestArchitecture::X86_64,
        };
        let (shared, shared_backings) = super::shared_backing::Factory::store(hl_memory::SharedLimits::default())
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .install_ipc(Arc::clone(&shared))
            .map_err(|_| EngineError::LaunchFailed)?;
        let arena = Arc::new(
            VirtualMemory::reserve_in(Arc::clone(&self.resources), arena::Capacity::DEFAULT)
                .map_err(|_| EngineError::LaunchFailed)?
                .with_shared_store(Arc::clone(&shared))
                .with_shared_backings(shared_backings),
        );
        let limits = LoadLimits {
            pie_hint: Some(0x10_00000),
            interpreter_hint: Some(0x20_00000),
            stack_hint: Some(0x30_00000),
            host_page_size: arena.page_size() as u64,
            ..LoadLimits::default()
        };
        let exec_limits = limits;
        let features = Self::guest_features(architecture);
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            Arc::clone(&shared),
            hl_memory::AddressSpaceId { slot: 1, generation: 1 },
        ));
        assembly
            .install_memory(Arc::new(hl_runtime::MemoryExit::new(Arc::clone(&mappings))))
            .map_err(|_| EngineError::LaunchFailed)?;
        let image_space = space::AddressSpace::new(Arc::clone(&arena), Arc::clone(&mappings));
        let source = match self.authority.as_ref() {
            Some(authority) => source::Source::Projected(source::ProjectedSource::new(Arc::clone(authority))),
            None => source::Source::File(match plan.rootfs.as_deref() {
                Some(rootfs) if plan.options.get("HL_OVERLAY_UPPER").is_some() => {
                    let lowers = plan
                        .options
                        .get("HL_LOWER")
                        .map(|records| records.lines().map(|lower| lower.as_bytes().to_vec()).collect())
                        .unwrap_or_default();
                    source::FileSource::layered(rootfs, lowers)
                }
                Some(rootfs) => source::FileSource::rooted(Some(rootfs)),
                None => source::FileSource::new(None),
            }),
        };
        let mut loader = Loader::new(source, exec_image::ImageSpace::new(Arc::clone(&image_space)), limits);
        let arguments = plan.arguments.iter().map(Vec::as_slice).collect::<Vec<_>>();
        // The Linux execution boundary preserves the caller's environment
        // byte-for-byte. Product/container defaults belong in the launch-plan
        // owner; synthesizing them here changes envp cardinality and ordering.
        let environment_refs = plan.environment.iter().map(Vec::as_slice).collect::<Vec<_>>();
        // Initial auxv identity must match the credentials installed by the
        // runtime route. Exec already projects the live task credentials.
        let uid = routing::launch_identity(plan, "HL_UID", routing::host_user())?;
        let gid = routing::launch_identity(plan, "HL_GID", routing::host_group())?;
        let loaded = loader
            .load(LoadRequest {
                architecture,
                image_path: path,
                executable_path: &guest_executable,
                arguments: &arguments,
                environment: &environment_refs,
                random: match self.entropy {
                    Some(entropy) => entropy,
                    None => self.entropy_source.read().map_err(|_| EngineError::LaunchFailed)?,
                },
                credentials: GuestCredentials {
                    user: uid,
                    effective_user: uid,
                    group: gid,
                    effective_group: gid,
                },
                features,
            })
            .map_err(EngineError::Load)?;
        let image_space_tls = loaded.initial_tls().clone();
        let auxiliary = image_data::AuxiliaryImage::encode(loaded.initial_stack());
        let (_, loaded_space) = loader.into_parts();
        if !Arc::ptr_eq(&loaded_space.space(), &image_space) {
            return Err(EngineError::LaunchFailed);
        }
        image_space.publish_procfs_image(&loaded, guest_executable.clone(), plan.environment.clone());
        let entry = loaded.dynamic_handoff().start_entry();
        let stack = loaded.initial_stack().stack_pointer();
        let cpu = match isa {
            GuestIsa::Aarch64 => {
                let mut cpu = Aarch64CpuState::default();
                cpu.pc = entry;
                cpu.sp = stack;
                ExecutionCpuSnapshot::Aarch64(cpu)
            }
            GuestIsa::X86_64 => {
                let mut cpu = CpuState::default();
                cpu.rip = entry;
                cpu.registers[4] = stack;
                ExecutionCpuSnapshot::X86_64(cpu)
            }
        };
        let snapshot = ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu,
            cache_epoch: 1,
            fault: None,
        };
        let routed = routing::create(
            Arc::clone(&arena),
            mappings,
            plan,
            assembly,
            architecture,
            Arc::clone(&cancellation),
            self.authority.clone(),
            self.entropy_source.clone(),
            &services.streams,
        )?;
        routed
            .process
            .space()
            .publish_procfs_image(&loaded, guest_executable.clone(), plan.environment.clone());
        routed
            .process
            .tasks()
            .publish_arguments(routed.process.process_id(), plan.arguments.clone())
            .map_err(|_| EngineError::LaunchFailed)?;
        routed.process.set_auxiliary(auxiliary)?;
        let execution_root = routing::image::WorkspaceRoot::select(plan);
        let load_context = Arc::new(exec_image::LoadContext::new(
            routed.process.tasks(),
            features,
            self.entropy_source.clone(),
        ));
        let loader_exec = Arc::new(hl_runtime::LoaderExecParticipant::new(
            architecture,
            exec_limits,
            source::Sources::new(execution_root.as_deref(), self.authority.clone()),
            exec_image::Spaces::new(Arc::clone(&image_space)),
            load_context.clone(),
            exec_image::Tls,
            exec_image::CpuImage,
            hl_runtime::LoaderExecImage {
                address_space: loaded_space,
                loaded,
                tls: image_space_tls,
                execution: snapshot.clone(),
            },
        ));
        let exec = Arc::new(exec_image::Coordinator::new(
            Arc::clone(&routed.process),
            Arc::clone(&threads),
            loader_exec,
            hl_runtime::TaskExecParticipant::new(routed.process.tasks()),
            execution_root,
            architecture,
            exec_limits,
            load_context,
            self.authority.clone(),
        ));
        routed
            .process
            .install_exec(&exec)
            .map_err(|_| EngineError::LaunchFailed)?;
        let exec_slot = assembly.exec_slot();
        exec_slot
            .register(routed.process.process(), exec)
            .map_err(|_| EngineError::LaunchFailed)?;
        routed
            .process
            .install_registration(exec_image::Registration::new(exec_slot, routed.process.process()))
            .map_err(|_| EngineError::LaunchFailed)?;
        let _initial_router = routed.router;
        let contexts = Arc::new(clone::Contexts::new(Arc::clone(&routed.process), Arc::clone(&threads)));
        let clone_runtime = contexts.build();
        contexts
            .install(Arc::clone(&clone_runtime))
            .map_err(|_| EngineError::LaunchFailed)?;
        routed
            .process
            .install_clone(&contexts)
            .map_err(|_| EngineError::LaunchFailed)?;
        let fork_runtime = fork::Runtime::new(Arc::clone(&routed.process), Arc::clone(&threads));
        routed
            .process
            .install_fork(&fork_runtime)
            .map_err(|_| EngineError::LaunchFailed)?;
        routed
            .process
            .install_threads(&threads)
            .map_err(|_| EngineError::LaunchFailed)?;
        let trap = hl_runtime::ThreadCloneTrap::new(clone_runtime, routed.thread);
        let router = Arc::new(
            routed
                .process
                .router(routed.thread, Arc::clone(&cancellation), Some(Box::new(trap))),
        );
        threads
            .prepare(
                routed.thread,
                routed.process.process_id(),
                Arc::clone(&router),
                cancellation,
                routed.process.space(),
            )
            .map_err(|_| EngineError::LaunchFailed)?;
        threads
            .stage(routed.thread, snapshot)
            .map_err(|_| EngineError::LaunchFailed)?
            .publish();
        let memfds = Arc::new(hl_runtime::MemfdBindings::new(routed.process.memfd_registry()?));
        // Tag 1 is the RuntimeMemfd file subtype. Other File-family codecs are
        // intentionally absent and therefore retain the previous reject behavior.
        let files = Arc::new(hl_runtime::FileObjectCatalog::rejecting().bind(1, memfds.clone()));
        assembly
            .prepare_checkpoint(Arc::new(hl_runtime::DescriptorCheckpointParticipant::new(
                assembly.checkpoint_descriptors(),
                Arc::new(
                    hl_runtime::DescriptorObjectCatalog::rejecting()
                        .bind(hl_descriptor::ObjectKind::File, files)
                        .bind(
                            hl_descriptor::ObjectKind::Pipe,
                            assembly.ipc_pipes().ok_or(EngineError::LaunchFailed)?.bindings(),
                        )
                        .bind(hl_descriptor::ObjectKind::Event, assembly.event_bindings())
                        .bind(hl_descriptor::ObjectKind::EventCounter, assembly.event_bindings())
                        .bind(hl_descriptor::ObjectKind::Poll, assembly.event_bindings())
                        .bind(hl_descriptor::ObjectKind::Socket, routed.process.network_bindings()),
                ),
            )))
            .map_err(|_| EngineError::LaunchFailed)?;
        let checkpoint_memory = Arc::new(hl_runtime::CheckpointMemoryState::new(Arc::new(
            hl_runtime::CheckpointMemory::new(routed.process.space().mappings(), Arc::clone(&shared)),
        )));
        assembly
            .prepare_checkpoint(Arc::new(
                hl_runtime::MemoryCheckpointParticipant::new(
                    checkpoint_memory.clone(),
                    Arc::new(checkpoint::Host::new(routed.process.space())),
                    Arc::new(hl_runtime::PortableMemoryCodec),
                )
                .with_resources(memfds),
            ))
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_provider_checkpoint()
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_event_checkpoint()
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_network_checkpoint(routed.process.network_bindings())
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_ipc_checkpoint(checkpoint_memory)
            .map_err(|_| EngineError::LaunchFailed)?;
        let machine = threads.find(routed.thread).ok_or(EngineError::LaunchFailed)?.machine;
        assembly
            .prepare_checkpoint(Arc::new(hl_runtime::ExecutionCheckpointParticipant::new(machine)))
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .finalize_checkpoint(hl_checkpoint::ImageLimits::new(
                8,
                arena::Capacity::MAXIMUM,
                arena::Capacity::MAXIMUM,
            ))
            .map_err(|_| EngineError::LaunchFailed)?;
        transfer::Operation::new(plan, assembly, services).apply()?;
        let waiters = waiter::Pool::new(&routed.process.tasks()).map_err(|()| EngineError::LaunchFailed)?;
        let result = Self::schedule(isa, plan, &threads, &waiters, self.host_faults.clone());
        // Every parked host wait must be interrupted before the fixed worker
        // set is joined, including successful exit-group termination.
        threads.cancel_all(9);
        waiters.stop();
        result
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
                if !threads.is_empty() {
                    return Ok(None);
                }
                exit
            }
            ThreadTerminal::Group(exit) => {
                threads.terminate_group(&run).map_err(Self::thread_error)?;
                if !threads.is_empty() {
                    return Ok(None);
                }
                exit
            }
        };
        diagnostics::TraceReport::new(plan, &run.router).write()?;
        Ok(Some(exit))
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
        };
        let mut opcode = [0_u8; 15];
        let opcode_len = if detail == 0 {
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
