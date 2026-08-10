use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::runtime_machine::{RustRuntimeFactory, RustRuntimeMachine};
use hl_runtime::RuntimeAssemblyConfig;
use std::sync::Arc;
#[cfg(feature = "c-execution")]
use std::sync::Mutex;

type RustMachine = RustRuntimeMachine<crate::native::GuestExecutor>;

pub(super) enum ProductionMachine {
    Rust(RustMachine),
    #[cfg(feature = "c-execution")]
    C(CMachine),
}

#[cfg(feature = "c-execution")]
pub(super) struct CMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launch_plan::RuntimeLaunchPlan,
    services: crate::composition::RuntimeServices,
    execution: Mutex<CExecutionState>,
}

#[cfg(feature = "c-execution")]
struct CExecutionState {
    prepared: Option<Arc<crate::c_execution::CGuestExecutor>>,
    current: Option<Arc<crate::c_execution::CGuestExecutor>>,
}

#[cfg(feature = "c-execution")]
impl CMachine {
    fn start(&self) -> Result<(), EngineError> {
        let execution = {
            let mut state = self.execution.lock().map_err(|_| EngineError::Synchronization)?;
            let execution = match state.prepared.take() {
                Some(execution) => execution,
                None => Arc::new(crate::c_execution::CGuestExecutor::create(
                    self.isa,
                    &self.plan,
                    &self.services,
                )?),
            };
            state.current = Some(Arc::clone(&execution));
            execution
        };
        execution.start_plan(&self.plan)
    }

    fn current(&self) -> Result<Arc<crate::c_execution::CGuestExecutor>, EngineError> {
        self.execution
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .current
            .clone()
            .ok_or(EngineError::NotStarted)
    }
}

pub(super) struct ProductionFactory;

impl RuntimeFactory for ProductionFactory {
    type Machine = ProductionMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        match request.plan.options.get("HL_EXECUTION_BACKEND") {
            None | Some("rust") => {
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
                .map(ProductionMachine::Rust)
            }
            #[cfg(feature = "c-execution")]
            Some("c") => {
                if request.isa != crate::activation::GuestIsa::Aarch64
                    || request.plan.options.get("HL_CHECKPOINT").is_some()
                    || request.plan.options.get("HL_RESTORE").is_some()
                {
                    return Err(CompositionError::RuntimeConstruction);
                }
                crate::c_execution::CGuestExecutor::create(request.isa, request.plan, request.services)
                    .map(|execution| {
                        ProductionMachine::C(CMachine {
                            isa: request.isa,
                            plan: request.plan.clone(),
                            services: request.services.clone(),
                            execution: Mutex::new(CExecutionState {
                                prepared: Some(Arc::new(execution)),
                                current: None,
                            }),
                        })
                    })
                    .map_err(|_| CompositionError::RuntimeConstruction)
            }
            Some(_) => Err(CompositionError::RuntimeConstruction),
        }
    }
}

impl GuestMachine for ProductionMachine {
    fn start(&self) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.start(),
            #[cfg(feature = "c-execution")]
            Self::C(machine) => machine.start(),
        }
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        match self {
            Self::Rust(machine) => machine.wait(),
            #[cfg(feature = "c-execution")]
            Self::C(machine) => Ok(machine.current()?.exit()),
        }
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.stop(request),
            #[cfg(feature = "c-execution")]
            Self::C(machine) => machine.current()?.stop_request(request),
        }
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.checkpoint_supported(),
            #[cfg(feature = "c-execution")]
            Self::C(_) => Err(EngineError::Unsupported),
        }
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.capture_checkpoint(),
            #[cfg(feature = "c-execution")]
            Self::C(_) => Err(EngineError::Unsupported),
        }
    }
}

#[cfg(all(test, feature = "c-execution"))]
mod tests {
    use crate::{
        activation::GuestIsa,
        composition::{CheckpointSink, CheckpointSource, CompositionError, StandardStreams},
        engine::EngineError,
        launch_plan::RuntimePlan,
        options::Options,
        runtime::Engine,
    };
    use std::sync::Arc;

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
    fn retained_backend_rejects_active_checkpoint_policy() {
        let mut plan = c_plan();
        plan.options.set("HL_RESTORE", "1", true).unwrap();
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
