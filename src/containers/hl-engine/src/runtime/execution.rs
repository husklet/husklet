use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, StopRequest};
use std::sync::Arc;
#[cfg(hl_retained_c)]
use std::sync::Mutex;

pub(crate) enum ProductionMachine {
    #[cfg(hl_retained_c)]
    C(Box<CMachine>),
}

#[cfg(hl_retained_c)]
pub(crate) struct CMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launch_plan::RuntimeLaunchPlan,
    services: crate::composition::RuntimeServices,
    execution: Mutex<CExecutionState>,
}

#[cfg(hl_retained_c)]
struct CExecutionState {
    prepared: Option<Arc<crate::execution::process::CWorker>>,
    current: Option<Arc<crate::execution::process::CWorker>>,
}

#[cfg(hl_retained_c)]
impl CMachine {
    fn start(&self) -> Result<(), EngineError> {
        let execution = {
            let mut state = self.execution.lock().map_err(|_| EngineError::Synchronization)?;
            let execution = match state.prepared.take() {
                Some(execution) => execution,
                None => Arc::new(crate::execution::process::CWorker::create(
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

    fn current(&self) -> Result<Arc<crate::execution::process::CWorker>, EngineError> {
        self.execution
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .current
            .clone()
            .ok_or(EngineError::NotStarted)
    }
}

pub(crate) struct ProductionFactory;

impl ProductionFactory {
    #[cfg(hl_retained_c)]
    fn c(request: RuntimeConstruction<'_>) -> Result<ProductionMachine, CompositionError> {
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
}

impl RuntimeFactory for ProductionFactory {
    type Machine = ProductionMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        #[cfg(hl_retained_c)]
        return Self::c(request);

        #[cfg(not(hl_retained_c))]
        Err(CompositionError::RuntimeConstruction)
    }
}

impl GuestMachine for ProductionMachine {
    fn start(&self) -> Result<(), EngineError> {
        match self {
            #[cfg(hl_retained_c)]
            Self::C(machine) => machine.start(),
        }
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        match self {
            #[cfg(hl_retained_c)]
            Self::C(machine) => machine.current()?.wait(),
        }
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        match self {
            #[cfg(hl_retained_c)]
            Self::C(machine) => machine.current()?.stop(request),
        }
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        match self {
            #[cfg(hl_retained_c)]
            Self::C(machine) => machine.current()?.checkpoint_supported(),
        }
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        match self {
            #[cfg(hl_retained_c)]
            Self::C(machine) => machine.current()?.capture_checkpoint(),
        }
    }
}

#[cfg(all(test, hl_retained_c))]
mod tests {
    use crate::{
        activation::GuestIsa,
        composition::{CheckpointSink, CheckpointSource, CompositionError, StandardStreams},
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
        RuntimePlan {
            rootfs: None,
            executable_host: None,
            arguments: vec![b"guest".to_vec()],
            environment: Vec::new(),
            result_path: None,
            options: Options::default(),
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
    fn retained_backend_constructs_both_compiled_guest_isas() {
        for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
            assert!(Engine::from_plan(isa, c_plan()).is_ok(), "failed to construct {isa:?}");
        }
    }

    #[test]
    fn retained_backend_selection_is_operator_visible() {
        let _guard = crate::execution::EVENT_CAPTURE_LOCK.lock().unwrap();
        let output = Arc::new(Mutex::new(String::new()));
        let tags = hl_log::Logging::global().tags();
        let level = hl_log::Logging::global().level();
        hl_log::Output::global().set(Box::new(LogCapture(Arc::clone(&output))));
        hl_log::Logging::global().set(hl_log::tag::EXEC);
        hl_log::Logging::global().set_level(hl_log::Level::Info);

        assert!(Engine::from_plan(GuestIsa::Aarch64, c_plan()).is_ok());

        hl_log::Logging::global().set(tags);
        hl_log::Logging::global().set_level(level);
        hl_log::Output::global().reset();
        assert!(
            output
                .lock()
                .unwrap()
                .contains("execution.backend.selected=c isa=Aarch64")
        );
    }

    #[test]
    fn product_default_constructs_both_compiled_guest_isas() {
        for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
            assert!(Engine::from_plan(isa, c_plan()).is_ok(), "failed to construct {isa:?}");
        }
    }

    #[test]
    fn retained_backend_constructs_active_checkpoint_policy() {
        for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
            for option in ["HL_CHECKPOINT", "HL_RESTORE"] {
                let mut plan = c_plan();
                plan.options.set(option, "1", true).unwrap();
                assert!(
                    Engine::with_checkpoint(isa, plan, StandardStreams::default(), Arc::new(Store), Arc::new(Store),)
                        .is_ok()
                );
            }
        }
    }

    #[test]
    fn product_default_constructs_checkpoint_policy() {
        for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
            for option in ["HL_CHECKPOINT", "HL_RESTORE"] {
                let mut plan = c_plan();
                plan.options.set(option, "1", true).unwrap();
                assert!(
                    Engine::with_checkpoint(isa, plan, StandardStreams::default(), Arc::new(Store), Arc::new(Store),)
                        .is_ok()
                );
            }
        }
    }

    #[test]
    fn production_build_has_no_backend_selector() {
        let mut plan = c_plan();
        assert!(plan.options.set("HL_EXECUTION_BACKEND", "rust", true).is_err());
        assert_eq!(plan.options.get("HL_EXECUTION_BACKEND"), None);
        assert!(Engine::from_plan(GuestIsa::Aarch64, plan).is_ok());
    }
}
