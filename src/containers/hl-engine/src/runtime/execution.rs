use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::runtime_machine::{RustRuntimeFactory, RustRuntimeMachine};
use hl_runtime::RuntimeAssemblyConfig;
use std::sync::Arc;
#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
use std::sync::Mutex;

type RustMachine = RustRuntimeMachine<crate::native::GuestExecutor>;

pub(super) enum ProductionMachine {
    Rust(Box<RustMachine>),
    #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
    C(Box<CMachine>),
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
pub(super) struct CMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launch_plan::RuntimeLaunchPlan,
    services: crate::composition::RuntimeServices,
    execution: Mutex<CExecutionState>,
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
struct CExecutionState {
    prepared: Option<Arc<crate::c_execution::process::CWorker>>,
    current: Option<Arc<crate::c_execution::process::CWorker>>,
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
impl CMachine {
    fn start(&self) -> Result<(), EngineError> {
        let execution = {
            let mut state = self.execution.lock().map_err(|_| EngineError::Synchronization)?;
            let execution = match state.prepared.take() {
                Some(execution) => execution,
                None => Arc::new(crate::c_execution::process::CWorker::create(
                    self.isa,
                    &self.plan,
                    &self.services,
                )?),
            };
            state.current = Some(Arc::clone(&execution));
            execution
        };
        execution.start()
    }

    fn current(&self) -> Result<Arc<crate::c_execution::process::CWorker>, EngineError> {
        self.execution
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .current
            .clone()
            .ok_or(EngineError::NotStarted)
    }
}

pub(super) struct ProductionFactory;

#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
#[derive(Clone, Copy)]
enum CUnsupported {
    GuestIsa,
    Checkpoint,
    Restore,
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
impl CUnsupported {
    const fn name(self) -> &'static str {
        match self {
            Self::GuestIsa => "guest_isa",
            Self::Checkpoint => "checkpoint",
            Self::Restore => "restore",
        }
    }
}

impl ProductionFactory {
    fn rust(request: RuntimeConstruction<'_>) -> Result<ProductionMachine, CompositionError> {
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "execution.backend.selected",
            backend = "rust",
            isa = ?request.isa
        );
        RustRuntimeFactory::new(
            Arc::new(crate::native::GuestExecutor::default()),
            Arc::new(super::Services),
            RuntimeAssemblyConfig::default(),
        )
        .construct(request)
        .map(Box::new)
        .map(ProductionMachine::Rust)
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
    fn c(request: RuntimeConstruction<'_>) -> Result<ProductionMachine, CompositionError> {
        if let Some(reason) = Self::c_unsupported(&request) {
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Error,
                "execution.backend.rejected",
                backend = "c",
                isa = ?request.isa,
                reason = reason.name()
            );
            return Err(CompositionError::RuntimeConstruction);
        }
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "execution.backend.selected",
            backend = "c",
            isa = ?request.isa
        );
        hl_log::hl_log!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "execution.backend.selected=c isa={:?}",
            request.isa
        );
        Ok(ProductionMachine::C(Box::new(CMachine {
            isa: request.isa,
            plan: request.plan.clone(),
            services: request.services.clone(),
            execution: Mutex::new(CExecutionState {
                prepared: None,
                current: None,
            }),
        })))
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
    fn c_unsupported(request: &RuntimeConstruction<'_>) -> Option<CUnsupported> {
        if request.isa != crate::activation::GuestIsa::Aarch64 {
            Some(CUnsupported::GuestIsa)
        } else if request.plan.options.get("HL_CHECKPOINT").is_some() {
            Some(CUnsupported::Checkpoint)
        } else if request.plan.options.get("HL_RESTORE").is_some() {
            Some(CUnsupported::Restore)
        } else {
            None
        }
    }
}

impl RuntimeFactory for ProductionFactory {
    type Machine = ProductionMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        match request.plan.options.get("HL_EXECUTION_BACKEND") {
            Some("rust") => Self::rust(request),
            #[cfg(not(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution")))]
            None => Self::rust(request),
            #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
            None => Self::c(request),
            #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
            Some("c") => Self::c(request),
            Some(_) => Err(CompositionError::RuntimeConstruction),
        }
    }
}

impl GuestMachine for ProductionMachine {
    fn start(&self) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.start(),
            #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
            Self::C(machine) => machine.start(),
        }
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        match self {
            Self::Rust(machine) => machine.wait(),
            #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
            Self::C(machine) => machine.current()?.wait(),
        }
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.stop(request),
            #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
            Self::C(machine) => machine.current()?.stop(request),
        }
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.checkpoint_supported(),
            #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
            Self::C(_) => Err(EngineError::Unsupported),
        }
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.capture_checkpoint(),
            #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
            Self::C(_) => Err(EngineError::Unsupported),
        }
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
mod tests {
    use crate::{
        activation::GuestIsa,
        composition::{CheckpointSink, CheckpointSource, CompositionError, StandardStreams},
        engine::EngineError,
        launch_plan::RuntimePlan,
        options::Options,
        runtime::Engine,
    };
    use std::sync::{Arc, Mutex};

    struct LogCapture(Arc<Mutex<String>>);

    impl hl_log::Sink for LogCapture {
        fn write_line(&self, line: &str) {
            self.0.lock().unwrap().push_str(line);
        }
    }

    fn c_plan() -> RuntimePlan {
        let mut options = Options::default();
        options.set("HL_EXECUTION_BACKEND", "c", true).unwrap();
        RuntimePlan {
            rootfs: None,
            executable_host: None,
            arguments: vec![b"guest".to_vec()],
            environment: Vec::new(),
            result_path: None,
            options,
        }
    }

    struct Store;

    impl CheckpointSink for Store {
        fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
            Ok(())
        }
    }

    impl CheckpointSource for Store {
        fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn retained_backend_rejects_uncompiled_guest_isa_early() {
        assert!(matches!(
            Engine::from_plan(GuestIsa::X86_64, c_plan()),
            Err(EngineError::LaunchFailed)
        ));
    }

    #[test]
    fn retained_backend_selection_is_operator_visible() {
        let output = Arc::new(Mutex::new(String::new()));
        hl_log::Output::global().set(Box::new(LogCapture(Arc::clone(&output))));
        hl_log::Logging::global().set(hl_log::tag::EXEC);
        hl_log::Logging::global().set_level(hl_log::Level::Info);

        assert!(Engine::from_plan(GuestIsa::Aarch64, c_plan()).is_ok());

        hl_log::Logging::global().set(hl_log::Tags::NONE);
        hl_log::Output::global().reset();
        assert!(
            output
                .lock()
                .unwrap()
                .contains("execution.backend.selected=c isa=Aarch64")
        );
    }

    #[test]
    fn product_default_rejects_uncompiled_guest_isa() {
        let mut plan = c_plan();
        plan.options.unset("HL_EXECUTION_BACKEND").unwrap();
        assert!(matches!(
            Engine::from_plan(GuestIsa::X86_64, plan),
            Err(EngineError::LaunchFailed)
        ));
    }

    #[test]
    fn retained_backend_rejects_active_checkpoint_policy() {
        for option in ["HL_CHECKPOINT", "HL_RESTORE"] {
            let mut plan = c_plan();
            plan.options.set(option, "1", true).unwrap();
            assert!(matches!(
                Engine::with_checkpoint(
                    GuestIsa::Aarch64,
                    plan,
                    StandardStreams::default(),
                    Arc::new(Store),
                    Arc::new(Store),
                ),
                Err(EngineError::LaunchFailed)
            ));
        }
    }

    #[test]
    fn product_default_rejects_checkpoint_policy() {
        for option in ["HL_CHECKPOINT", "HL_RESTORE"] {
            let mut plan = c_plan();
            plan.options.unset("HL_EXECUTION_BACKEND").unwrap();
            plan.options.set(option, "1", true).unwrap();
            assert!(matches!(
                Engine::with_checkpoint(
                    GuestIsa::Aarch64,
                    plan,
                    StandardStreams::default(),
                    Arc::new(Store),
                    Arc::new(Store),
                ),
                Err(EngineError::LaunchFailed)
            ));
        }
    }
}
