#![allow(unsafe_code)]

use crate::composition::{
    CheckpointSink, CheckpointSource, CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory,
};
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use std::ffi::CString;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use super::checkpoint::Server;

#[cfg(unix)]
use super::terminal::NativeOutputBridge;
#[cfg(unix)]
use super::terminal::NativeTerminalBridge;

const REQUEST_INTERRUPT: u32 = 1;
const REQUEST_FORCE_STOP: u32 = 2;
const REQUEST_SIGNAL: u32 = 3;
const REQUEST_CHECKPOINT: u32 = 4;

pub(crate) struct ProductionMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launcher::plan::RuntimePlan,
    #[cfg(unix)]
    terminal: Option<NativeTerminalBridge>,
    #[cfg(unix)]
    output: Option<NativeOutputBridge>,
    #[cfg(unix)]
    checkpoint: Option<CheckpointControl>,
    engine: Mutex<Option<Arc<hl_native::Engine>>>,
}

pub(crate) struct ProductionFactory;

impl RuntimeFactory for ProductionFactory {
    type Machine = ProductionMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        #[cfg(unix)]
        let terminal = request
            .services
            .streams
            .terminal()
            .map(NativeTerminalBridge::attach)
            .transpose()?;
        #[cfg(unix)]
        let output = if terminal.is_none() {
            request
                .services
                .streams
                .output()
                .map(NativeOutputBridge::attach)
                .transpose()?
        } else {
            None
        };
        #[cfg(unix)]
        let checkpoint = match (
            request.services.checkpoint_sink.clone(),
            request.services.checkpoint_source.clone(),
        ) {
            (Some(sink), Some(source)) => Some(CheckpointControl::start(sink, source)?),
            (None, None) => None,
            _ => return Err(CompositionError::RuntimeConstruction),
        };
        Ok(ProductionMachine {
            isa: request.isa,
            plan: request.plan.clone(),
            #[cfg(unix)]
            terminal,
            #[cfg(unix)]
            output,
            #[cfg(unix)]
            checkpoint,
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
            encode_environment_record(&mut encoded, record);
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
        options.push((
            CString::new("HL_GUEST_ENV").expect("literal"),
            CString::new(self.encode_environment()).map_err(|_| EngineError::LaunchFailed)?,
        ));
        options.push((
            CString::new("HL_GUEST_ENV_ESC").expect("literal"),
            CString::new("1").expect("literal"),
        ));
        options.push((
            CString::new("HL_GUEST_ENV_EXACT").expect("literal"),
            CString::new("1").expect("literal"),
        ));
        let names = options.iter().map(|(name, _)| name.as_ptr()).collect::<Vec<_>>();
        let values = options.iter().map(|(_, value)| value.as_ptr()).collect::<Vec<_>>();
        #[cfg(unix)]
        let standard_fds = self
            .terminal
            .as_ref()
            .map(NativeTerminalBridge::standard_fds)
            .or_else(|| self.output.as_ref().map(NativeOutputBridge::standard_fds))
            .unwrap_or([0, 1, 2]);
        #[cfg(not(unix))]
        let standard_fds = [0, 1, 2];
        let config = hl_native::EngineConfig {
            isa: match self.isa {
                crate::activation::GuestIsa::Aarch64 => 1,
                crate::activation::GuestIsa::X86_64 => 2,
            },
            rootfs: rootfs.as_deref(),
            executable_host: executable.as_deref(),
            executable_fd: -1,
            option_names: &names,
            option_values: &values,
            standard_fds,
            provider_fd: -1,
        };
        // SAFETY: all pointers in config remain live for this call and there is no callback state.
        let engine = unsafe { hl_native::Engine::create(config) }.map_err(|_| EngineError::LaunchFailed)?;
        #[cfg(unix)]
        if let Some(checkpoint) = &self.checkpoint {
            engine
                .configure_checkpoint(&checkpoint.transport)
                .map_err(|_| EngineError::LaunchFailed)?;
        }
        Ok(engine)
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

fn encode_environment_record(encoded: &mut Vec<u8>, record: &[u8]) {
    for byte in record {
        match byte {
            b'\\' => encoded.extend_from_slice(b"\\\\"),
            b'\n' => encoded.extend_from_slice(b"\\n"),
            byte => encoded.push(*byte),
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
        engine.run(&pointers).map_err(|_| EngineError::LaunchFailed)?;
        #[cfg(unix)]
        if let Some(output) = &self.output {
            output.flush();
        }
        Ok(())
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        let engine = self.current()?;
        Ok(Self::exit(engine.as_ref()))
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        let (kind, signal) = match request {
            StopRequest::Interrupt => (REQUEST_INTERRUPT, request.signal()),
            StopRequest::Force => (REQUEST_FORCE_STOP, request.signal()),
            StopRequest::Signal(signal) => (REQUEST_SIGNAL, signal),
        };
        self.current()?
            .request(kind, signal)
            .map_err(|_| EngineError::StopFailed)
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        #[cfg(unix)]
        if self.checkpoint.is_some() {
            return Ok(());
        }
        Err(EngineError::Unsupported)
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        #[cfg(unix)]
        if let Some(checkpoint) = &self.checkpoint {
            let engine = self.current()?;
            return checkpoint.capture(engine.as_ref(), self.isa);
        }
        Err(EngineError::Unsupported)
    }
}

#[cfg(unix)]
struct CheckpointControl {
    server: Arc<Server>,
    transport: hl_native::CheckpointTransport,
    acceptor: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl CheckpointControl {
    fn start(sink: Arc<dyn CheckpointSink>, source: Arc<dyn CheckpointSource>) -> Result<Self, CompositionError> {
        let (broker, transport) =
            hl_native::CheckpointTransport::create().map_err(|_| CompositionError::RuntimeConstruction)?;
        let server = Arc::new(Server::new(sink, source));
        let acceptor = Server::start(&server, broker);
        Ok(Self {
            server,
            transport,
            acceptor: Some(acceptor),
        })
    }

    fn capture(&self, engine: &hl_native::Engine, isa: crate::activation::GuestIsa) -> Result<(), EngineError> {
        use std::time::{Duration, Instant};

        let _ = self.transport.bump();
        let signal = hl_native::CheckpointTransport::interrupt_signal(match isa {
            crate::activation::GuestIsa::Aarch64 => 1,
            crate::activation::GuestIsa::X86_64 => 2,
        });
        if signal <= 0 || engine.request(REQUEST_CHECKPOINT, 0).is_err() {
            return Err(EngineError::StopFailed);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut next_interrupt = Instant::now() + Duration::from_millis(100);
        loop {
            if self.server.committed() {
                return Ok(());
            }
            if self.server.failure().is_some() {
                return Err(EngineError::LaunchFailed);
            }
            if Instant::now() >= deadline {
                return Err(EngineError::WaitFailed);
            }
            if Instant::now() >= next_interrupt {
                let _ = engine.request(REQUEST_CHECKPOINT, 0);
                next_interrupt = Instant::now() + Duration::from_millis(100);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(unix)]
impl Drop for CheckpointControl {
    fn drop(&mut self) {
        self.server.stop();
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}
