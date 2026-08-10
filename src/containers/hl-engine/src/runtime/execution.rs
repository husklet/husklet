use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::runtime_machine::{RustRuntimeFactory, RustRuntimeMachine};
use hl_runtime::RuntimeAssemblyConfig;
use std::sync::Arc;

type RustMachine = RustRuntimeMachine<crate::native::GuestExecutor>;

pub(super) enum ProductionMachine {
    Rust(RustMachine),
    #[cfg(feature = "c-execution")]
    C(CMachine),
}

#[cfg(feature = "c-execution")]
pub(super) struct CMachine {
    execution: crate::c_execution::CGuestExecutor,
    plan: crate::launch_plan::RuntimeLaunchPlan,
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
            Some("c") => crate::c_execution::CGuestExecutor::create(request.isa, request.plan)
                .map(|execution| {
                    ProductionMachine::C(CMachine {
                        execution,
                        plan: request.plan.clone(),
                    })
                })
                .map_err(|_| CompositionError::RuntimeConstruction),
            Some(_) => Err(CompositionError::RuntimeConstruction),
        }
    }
}

impl GuestMachine for ProductionMachine {
    fn start(&self) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.start(),
            #[cfg(feature = "c-execution")]
            Self::C(machine) => machine.execution.start_plan(&machine.plan),
        }
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        match self {
            Self::Rust(machine) => machine.wait(),
            #[cfg(feature = "c-execution")]
            Self::C(machine) => Ok(machine.execution.exit()),
        }
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        match self {
            Self::Rust(machine) => machine.stop(request),
            #[cfg(feature = "c-execution")]
            Self::C(machine) => machine.execution.stop_request(request),
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
