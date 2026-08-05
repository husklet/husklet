use super::*;

#[test]
fn task_checkpoint_setup_is_idempotent() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    assert!(!assembly.has_checkpoint_role(hl_runtime::CheckpointRole::Task));
    prepare_tasks(&assembly).unwrap();
    prepare_tasks(&assembly).unwrap();
    assert!(assembly.has_checkpoint_role(hl_runtime::CheckpointRole::Task));
}
use crate::composition::{ActivationChannel, EngineBackend, RuntimeServices};
use crate::engine::{ExitKind, Workspace, WorkspaceId};
use crate::options::Options;
use std::sync::Mutex;

#[path = "guest_image.rs"]
mod guest_image;

static STAGED_IMAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct StagedImage {
    root: std::path::PathBuf,
    executable: std::path::PathBuf,
}

impl StagedImage {
    fn create(image: &[u8]) -> Self {
        let identity = STAGED_IMAGE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("hl-machine-stage-{}-{identity}", std::process::id(),));
        std::fs::create_dir(&root).unwrap();
        let executable = root.join("guest");
        std::fs::write(&executable, image).unwrap();
        Self { root, executable }
    }
}

impl Drop for StagedImage {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[derive(Default)]
struct Activation;

impl ActivationChannel for Activation {
    fn send(&self, _: &[u8]) -> Result<(), CompositionError> {
        Ok(())
    }

    fn receive(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct Host {
    validations: Mutex<usize>,
    fork: bool,
    exec: bool,
}

impl RuntimeHostServices for Host {
    fn exec_port(&self, _: &RuntimeAssembly) -> Result<Option<Arc<dyn hl_runtime::RuntimeExecPort>>, CompositionError> {
        Ok(self
            .exec
            .then(|| Arc::new(hl_runtime::RejectingExecPort) as Arc<dyn hl_runtime::RuntimeExecPort>))
    }

    fn fork_port(&self, _: &RuntimeAssembly) -> Result<Option<Arc<dyn hl_runtime::RuntimeForkPort>>, CompositionError> {
        Ok(self
            .fork
            .then(|| Arc::new(hl_runtime::RejectingForkPort) as Arc<dyn hl_runtime::RuntimeForkPort>))
    }

    fn validate(&self, assembly: &RuntimeAssembly) -> Result<(), CompositionError> {
        assembly
            .require(RuntimeDomain::Task)
            .map_err(|_| CompositionError::RuntimeConstruction)?;
        *self.validations.lock().unwrap() += 1;
        Ok(())
    }
}

#[derive(Default)]
struct ExecutionState {
    starts: Vec<(GuestIsa, Vec<Vec<u8>>, usize)>,
    stops: Vec<StopRequest>,
}

#[derive(Default)]
struct Execution {
    state: Mutex<ExecutionState>,
    fail_start: bool,
}

impl GuestExecutionPort for Execution {
    fn start(
        &self,
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        assembly: &RuntimeAssembly,
        _: &RuntimeServices,
    ) -> Result<(), EngineError> {
        if self.fail_start {
            return Err(EngineError::LaunchFailed);
        }
        let task_identity = Arc::as_ptr(&assembly.tasks()) as usize;
        self.state
            .lock()
            .unwrap()
            .starts
            .push((isa, plan.arguments.clone(), task_identity));
        Ok(())
    }

    fn wait(&self, _: &RuntimeAssembly) -> Result<EngineExit, EngineError> {
        let signal = self
            .state
            .lock()
            .unwrap()
            .stops
            .last()
            .map_or(0, |request| request.signal());
        Ok(EngineExit {
            kind: if signal == 0 { ExitKind::Code } else { ExitKind::Signal },
            guest_status: signal,
            detail: 0,
            fault: None,
        })
    }

    fn stop(&self, _: &RuntimeAssembly, request: StopRequest) -> Result<(), EngineError> {
        self.state.lock().unwrap().stops.push(request);
        Ok(())
    }
}

#[test]
fn start_failure_has_bounded_construction_cause() {
    let factory = RustRuntimeFactory::new(
        Arc::new(Execution {
            fail_start: true,
            ..Execution::default()
        }),
        Arc::new(Host::default()),
        RuntimeAssemblyConfig::default(),
    );
    let machine = factory
        .construct(RuntimeConstruction {
            isa: GuestIsa::Aarch64,
            plan: &Fixture::plan(b"guest", None),
            services: &Fixture::services(),
        })
        .unwrap();
    assert_eq!(
        machine.start(),
        Err(EngineError::Construction(ConstructionError::Start))
    );
}

#[derive(Clone, Copy)]
struct Workspaces;

impl Workspace for Workspaces {
    fn prepare(&self) -> Result<WorkspaceId, EngineError> {
        Ok(WorkspaceId(1))
    }

    fn cleanup(&self, _: WorkspaceId) -> Result<(), EngineError> {
        Ok(())
    }
}

struct Fixture;

impl Fixture {
    fn plan(argument: &[u8], pid_limit: Option<&str>) -> RuntimeLaunchPlan {
        let mut options = Options::default();
        if let Some(pid_limit) = pid_limit {
            options.set("HL_PIDS_MAX", pid_limit, true).unwrap();
        }
        RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: vec![argument.to_vec()],
            environment: Vec::new(),
            result_path: None,
            options,
        }
    }

    fn services() -> RuntimeServices {
        RuntimeServices {
            activation: Arc::new(Activation),
            checkpoint_sink: None,
            checkpoint_source: None,
            streams: crate::composition::StandardStreams::default(),
        }
    }
}

#[test]
fn concrete_factory_propagates() {
    let execution = Arc::new(Execution::default());
    let host = Arc::new(Host::default());
    let factory = RustRuntimeFactory::new(
        Arc::clone(&execution),
        Arc::clone(&host),
        RuntimeAssemblyConfig::default(),
    );
    let backend = EngineBackend::construct(
        GuestIsa::Aarch64,
        Fixture::plan(b"arm", Some("8")),
        Fixture::services(),
        &factory,
        Workspaces,
    )
    .unwrap();
    backend.start().unwrap();
    backend.stop(StopRequest::Interrupt).unwrap();
    assert_eq!(backend.wait().unwrap().guest_status, 2);
    let state = execution.state.lock().unwrap();
    assert_eq!(state.starts[0].0, GuestIsa::Aarch64);
    assert_eq!(state.starts[0].1, [b"arm".to_vec()]);
    assert_eq!(*host.validations.lock().unwrap(), 1);
}

#[test]
fn arm_and_x86() {
    let execution = Arc::new(Execution::default());
    let host = Arc::new(Host::default());
    let factory = RustRuntimeFactory::new(Arc::clone(&execution), host, RuntimeAssemblyConfig::default());
    let arm = EngineBackend::construct(
        GuestIsa::Aarch64,
        Fixture::plan(b"arm", None),
        Fixture::services(),
        &factory,
        Workspaces,
    )
    .unwrap();
    let x86 = EngineBackend::construct(
        GuestIsa::X86_64,
        Fixture::plan(b"x86", None),
        Fixture::services(),
        &factory,
        Workspaces,
    )
    .unwrap();
    arm.start().unwrap();
    x86.start().unwrap();
    arm.wait().unwrap();
    x86.wait().unwrap();
    let state = execution.state.lock().unwrap();
    let arm_start = state.starts.iter().find(|start| start.0 == GuestIsa::Aarch64).unwrap();
    let x86_start = state.starts.iter().find(|start| start.0 == GuestIsa::X86_64).unwrap();
    assert_eq!(arm_start.1, [b"arm".to_vec()]);
    assert_eq!(x86_start.1, [b"x86".to_vec()]);
    assert_ne!(arm_start.2, x86_start.2);
}

#[test]
fn missing_checkpoint_and() {
    let execution = Arc::new(Execution::default());
    let factory = RustRuntimeFactory::new(execution, Arc::new(Host::default()), RuntimeAssemblyConfig::default());
    let machine = factory
        .construct(RuntimeConstruction {
            isa: GuestIsa::Aarch64,
            plan: &Fixture::plan(b"guest", None),
            services: &Fixture::services(),
        })
        .unwrap();
    assert_eq!(machine.checkpoint_supported(), Err(EngineError::Unsupported));
    assert_eq!(machine.fork_supported(), Err(EngineError::Unsupported));
}

#[test]
fn execution_installs_checkpoint_roles() {
    use std::os::unix::ffi::OsStrExt;

    let staged = StagedImage::create(&guest_image::aarch64_exit_image());
    let stage_root = staged.root.clone();
    let encoded = staged.executable.as_os_str().as_bytes().to_vec();
    let plan = RuntimeLaunchPlan {
        rootfs: None,
        executable_host: Some(encoded.clone()),
        arguments: vec![encoded],
        environment: Vec::new(),
        result_path: None,
        options: Options::default(),
    };
    let execution = Arc::new(crate::ffi::linux::GuestExecutor::default());
    let factory = RustRuntimeFactory::new(execution, Arc::new(Host::default()), RuntimeAssemblyConfig::default());
    let machine = factory
        .construct(RuntimeConstruction {
            isa: GuestIsa::Aarch64,
            plan: &plan,
            services: &Fixture::services(),
        })
        .unwrap();
    machine.start().unwrap();
    let roles = machine.assembly.checkpoint().unwrap().roles();
    assert!(roles.contains(&hl_runtime::CheckpointRole::Provider));
    assert!(roles.contains(&hl_runtime::CheckpointRole::Event));
    assert!(roles.contains(&hl_runtime::CheckpointRole::Network));
    assert!(roles.contains(&hl_runtime::CheckpointRole::Ipc));
    // `GuestExecutionPort::start` is synchronous and this fixture exits
    // immediately. Its terminal task state is not a live checkpoint subject;
    // this unit test owns only the composition contract. Live capture belongs
    // to the repository checkpoint scenario, where execution is held at an
    // explicit guest rendezvous.
    machine.wait().unwrap();
    drop(machine);
    drop(staged);
    assert!(!stage_root.exists());
}

#[test]
fn configured_fork_capability() {
    let factory = RustRuntimeFactory::new(
        Arc::new(Execution::default()),
        Arc::new(Host {
            validations: Mutex::new(0),
            fork: true,
            exec: false,
        }),
        RuntimeAssemblyConfig::default(),
    );
    let machine = factory
        .construct(RuntimeConstruction {
            isa: GuestIsa::Aarch64,
            plan: &Fixture::plan(b"guest", None),
            services: &Fixture::services(),
        })
        .unwrap();
    assert_eq!(machine.fork_supported(), Ok(()));
    let separate = RustRuntimeFactory::new(
        Arc::new(Execution::default()),
        Arc::new(Host::default()),
        RuntimeAssemblyConfig::default(),
    )
    .construct(RuntimeConstruction {
        isa: GuestIsa::Aarch64,
        plan: &Fixture::plan(b"guest", None),
        services: &Fixture::services(),
    })
    .unwrap();
    assert_eq!(separate.fork_supported(), Err(EngineError::Unsupported));
    assert_eq!(separate.exec_supported(), Err(EngineError::Unsupported));
}

#[test]
fn configured_exec_capability() {
    let factory = RustRuntimeFactory::new(
        Arc::new(Execution::default()),
        Arc::new(Host {
            validations: Mutex::new(0),
            fork: false,
            exec: true,
        }),
        RuntimeAssemblyConfig::default(),
    );
    let machine = factory
        .construct(RuntimeConstruction {
            isa: GuestIsa::Aarch64,
            plan: &Fixture::plan(b"guest", None),
            services: &Fixture::services(),
        })
        .unwrap();
    assert_eq!(machine.exec_supported(), Ok(()));
}

#[test]
fn invalid_projected_capacity() {
    let execution = Arc::new(Execution::default());
    let host = Arc::new(Host::default());
    let factory = RustRuntimeFactory::new(execution, Arc::clone(&host), RuntimeAssemblyConfig::default());
    let result = factory.construct(RuntimeConstruction {
        isa: GuestIsa::X86_64,
        plan: &Fixture::plan(b"guest", Some("0")),
        services: &Fixture::services(),
    });
    assert!(matches!(
        result,
        Err(CompositionError::Construction(ConstructionError::Assembly))
    ));
    assert_eq!(*host.validations.lock().unwrap(), 0);
}

#[test]
fn untrusted_descriptor_capacity() {
    let factory = RustRuntimeFactory::new(
        Arc::new(Execution::default()),
        Arc::new(Host::default()),
        RuntimeAssemblyConfig::default(),
    );
    let mut plan = Fixture::plan(b"guest", None);
    plan.options.set("HL_UNTRUSTED", "1", true).unwrap();
    let (config, _) = factory.assembly_config(&plan).unwrap();
    assert_eq!(config.maximum_processes, 65);
    assert_eq!(config.descriptor_limit, 1024);
}

#[test]
fn deterministic_host_services() {
    let execution = Arc::new(Execution::default());
    let host = Arc::new(hl_fake_host::FakeHost::new(91));
    let factory = RustRuntimeFactory::new(execution, Arc::clone(&host), RuntimeAssemblyConfig::default());
    let machine = factory.construct(RuntimeConstruction {
        isa: GuestIsa::Aarch64,
        plan: &Fixture::plan(b"guest", None),
        services: &Fixture::services(),
    });
    assert!(machine.is_ok());
    assert_eq!(host.transcript()[0].operation, "validate");
    assert_eq!(host.transcript()[0].resource, 91);
}
