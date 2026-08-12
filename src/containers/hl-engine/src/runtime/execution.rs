use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use std::ffi::CString;
use std::sync::{Arc, Mutex};

const REQUEST_INTERRUPT: u32 = 1;
const REQUEST_FORCE_STOP: u32 = 2;
const REQUEST_SIGNAL: u32 = 3;

pub(crate) struct ProductionMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launch_plan::RuntimeLaunchPlan,
    engine: Mutex<Option<Arc<hl_native::Engine>>>,
}

pub(crate) struct ProductionFactory;

impl RuntimeFactory for ProductionFactory {
    type Machine = ProductionMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        Ok(ProductionMachine {
            isa: request.isa,
            plan: request.plan.clone(),
            engine: Mutex::new(None),
        })
    }
}

impl ProductionMachine {
    fn encode_environment(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        for (index, record) in self.plan.environment.iter().enumerate() {
            if index != 0 {
                encoded.push(b'\n');
            }
            for byte in record {
                match byte {
                    b'\\' => encoded.extend_from_slice(b"\\\\"),
                    b'\n' => encoded.extend_from_slice(b"\\n"),
                    byte => encoded.push(*byte),
                }
            }
        }
        encoded
    }

    fn create(&self) -> Result<hl_native::Engine, EngineError> {
        let rootfs = self
            .plan
            .rootfs
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| EngineError::LaunchFailed)?;
        let executable = self
            .plan
            .executable_host
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| EngineError::LaunchFailed)?;
        let mut options = self
            .plan
            .options
            .iter()
            .map(|(name, value)| Ok((CString::new(name)?, CString::new(value)?)))
            .collect::<Result<Vec<_>, std::ffi::NulError>>()
            .map_err(|_| EngineError::LaunchFailed)?;
        options.push((CString::new("HL_GUEST_ENV").expect("literal"), CString::new(self.encode_environment()).map_err(|_| EngineError::LaunchFailed)?));
        options.push((CString::new("HL_GUEST_ENV_ESC").expect("literal"), CString::new("1").expect("literal")));
        options.push((CString::new("HL_GUEST_ENV_EXACT").expect("literal"), CString::new("1").expect("literal")));
        let names = options.iter().map(|(name, _)| name.as_ptr()).collect::<Vec<_>>();
        let values = options.iter().map(|(_, value)| value.as_ptr()).collect::<Vec<_>>();
        let config = hl_native::Create {
            isa: match self.isa {
                crate::activation::GuestIsa::Aarch64 => 1,
                crate::activation::GuestIsa::X86_64 => 2,
            },
            rootfs: rootfs.as_deref(),
            executable_host: executable.as_deref(),
            executable_fd: -1,
            option_names: &names,
            option_values: &values,
            standard_fds: [0, 1, 2],
            provider_fd: -1,
            syscall_context: std::ptr::null_mut(),
            syscall_dispatch: None,
        };
        // SAFETY: all pointers in config remain live for this call and there is no callback state.
        unsafe { hl_native::Engine::create(config) }.map_err(|_| EngineError::LaunchFailed)
    }

    fn current(&self) -> Result<Arc<hl_native::Engine>, EngineError> {
        self.engine
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .clone()
            .ok_or(EngineError::NotStarted)
    }

    fn exit(engine: &hl_native::Engine) -> EngineExit {
        let exit = engine.exit();
        EngineExit {
            kind: match exit.kind {
                1 => ExitKind::Code,
                2 => ExitKind::Signal,
                3 => ExitKind::Fault,
                _ => ExitKind::EngineError,
            },
            guest_status: exit.status,
            detail: exit.detail,
            fault: None,
        }
    }
}

impl GuestMachine for ProductionMachine {
    fn start(&self) -> Result<(), EngineError> {
        let engine = Arc::new(self.create()?);
        *self.engine.lock().map_err(|_| EngineError::Synchronization)? = Some(Arc::clone(&engine));
        let arguments = self
            .plan
            .arguments
            .iter()
            .map(|argument| CString::new(argument.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers = arguments.iter().map(|argument| argument.as_ptr()).collect::<Vec<_>>();
        engine.run(&pointers).map_err(|_| EngineError::LaunchFailed)
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        Ok(Self::exit(&self.current()?))
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        let (kind, signal) = match request {
            StopRequest::Interrupt => (REQUEST_INTERRUPT, request.signal()),
            StopRequest::Force => (REQUEST_FORCE_STOP, request.signal()),
            StopRequest::Signal(signal) => (REQUEST_SIGNAL, signal),
        };
        self.current()?.request(kind, signal).map_err(|_| EngineError::StopFailed)
    }
}
