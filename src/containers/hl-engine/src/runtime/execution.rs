#![allow(unsafe_code)]

#[cfg(unix)]
use crate::composition::{CheckpointSink, CheckpointSource};
use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
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
#[cfg(unix)]
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
        let engine = unsafe { hl_native::Engine::create(config) }.map_err(EngineError::NativeCreateFailed)?;
        #[cfg(unix)]
        let mut engine = engine;
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
        #[cfg(unix)]
        let recovery = if self.plan.options.get_bytes("HL_RESTORE").is_some() {
            let checkpoint = self.checkpoint.as_ref().ok_or(EngineError::LaunchFailed)?;
            Some(
                checkpoint
                    .begin_recovery(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)?,
            )
        } else {
            None
        };
        let engine = Arc::new(self.create()?);
        *self.engine.lock().map_err(|_| EngineError::Synchronization)? = Some(Arc::clone(&engine));
        let arguments = self
            .plan
            .arguments
            .iter()
            .map(|argument| CString::new(argument.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers = arguments.iter().map(|argument| argument.as_ptr()).collect::<Vec<_>>();
        #[cfg(unix)]
        let run = if let Some(recovery) = recovery.as_ref() {
            // Recovery publication is completed by the restored process while
            // `run` is still waiting for that process to exit. Waiting only
            // after `run` returns lets a later checkpoint reuse the server
            // state first; the stale recovery waiter then observes that newer
            // generation and reports `Busy` despite a successful checkpoint.
            std::thread::scope(|scope| {
                let waiting = scope.spawn(|| recovery.wait());
                let run = engine.run(&pointers).map_err(native_run_failure);
                let restored = waiting.join().map_err(|_| EngineError::WaitFailed)?;
                run.and(restored)
            })
        } else {
            engine.run(&pointers).map_err(native_run_failure)
        };
        #[cfg(not(unix))]
        let run = engine.run(&pointers).map_err(native_run_failure);
        run?;
        #[cfg(unix)]
        if let Some(terminal) = &self.terminal {
            terminal.flush();
        }
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
            .map_err(EngineError::NativeStopFailed)
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        #[cfg(unix)]
        if self.checkpoint.is_some() {
            return Ok(());
        }
        Err(EngineError::Unsupported)
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.capture_checkpoint_until(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)
    }

    fn capture_checkpoint_until(&self, deadline: std::time::Instant) -> Result<(), EngineError> {
        #[cfg(not(unix))]
        let _ = deadline;
        #[cfg(unix)]
        if let Some(checkpoint) = &self.checkpoint {
            let engine = self.current()?;
            return checkpoint.capture(engine.as_ref(), self.isa, deadline);
        }
        Err(EngineError::Unsupported)
    }
}

fn native_run_failure(status: i32) -> EngineError {
    EngineError::NativeRunFailed(status)
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

    fn capture(
        &self,
        engine: &hl_native::Engine,
        isa: crate::activation::GuestIsa,
        deadline: std::time::Instant,
    ) -> Result<(), EngineError> {
        use std::time::{Duration, Instant};

        if Instant::now() >= deadline {
            return Err(EngineError::WaitFailed);
        }
        self.server
            .wait_capture_ready(deadline)
            .map_err(Self::capture_failure)?;
        let capture = self
            .server
            .begin_capture_after_admission(deadline, || self.transport.bump())
            .map_err(Self::capture_failure)?;
        let signal = hl_native::CheckpointTransport::interrupt_signal(match isa {
            crate::activation::GuestIsa::Aarch64 => 1,
            crate::activation::GuestIsa::X86_64 => 2,
        });
        if signal <= 0 || engine.request(REQUEST_CHECKPOINT, signal).is_err() {
            self.server
                .abort_capture(capture)
                .map_err(|_| EngineError::LaunchFailed)?;
            return Err(EngineError::StopFailed);
        }
        let mut next_interrupt = Instant::now() + Duration::from_millis(100);
        loop {
            let result = self
                .server
                .wait_capture(capture, next_interrupt)
                .map_err(|_| EngineError::LaunchFailed)?;
            if let Some(result) = result {
                return result.map_err(|failure| Self::capture_failure_with_exit(engine, failure));
            }
            if Instant::now() >= next_interrupt {
                let _ = engine.request(REQUEST_CHECKPOINT, signal);
                next_interrupt = Instant::now() + Duration::from_millis(100);
            }
        }
    }

    fn begin_recovery(&self, deadline: std::time::Instant) -> Result<RecoveryAdmission, EngineError> {
        let id = self
            .server
            .begin_recovery_after_admission(deadline, || self.transport.bump())
            .map_err(Self::capture_failure)?;
        Ok(RecoveryAdmission {
            server: Arc::clone(&self.server),
            id,
        })
    }

    fn capture_failure(failure: super::checkpoint::CaptureFailure) -> EngineError {
        match failure {
            super::checkpoint::CaptureFailure::Busy => EngineError::Busy,
            super::checkpoint::CaptureFailure::Deadline => EngineError::WaitFailed,
            super::checkpoint::CaptureFailure::Failed | super::checkpoint::CaptureFailure::Poisoned => {
                EngineError::LaunchFailed
            }
        }
    }

    fn capture_failure_with_exit(
        engine: &hl_native::Engine,
        failure: super::checkpoint::CaptureFailure,
    ) -> EngineError {
        // A channel EOF is observed just before the native child-reaper publishes its status. Give that
        // publication a tightly bounded opportunity to win so a host SIGSEGV is reported as Signal(11)
        // instead of the generic capture failure that triggered this diagnostic path.
        for _ in 0..10 {
            let native = engine.exit();
            if native.kind != 0 {
                return EngineError::CheckpointExited(ProductionMachine::exit(engine));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Self::capture_failure(failure)
    }
}

#[cfg(unix)]
struct RecoveryAdmission {
    server: Arc<Server>,
    id: u64,
}

#[cfg(unix)]
impl Drop for RecoveryAdmission {
    fn drop(&mut self) {
        let _ = self.server.abort_recovery(self.id);
    }
}

#[cfg(unix)]
impl RecoveryAdmission {
    fn wait(&self) -> Result<(), EngineError> {
        self.server
            .wait_recovery(self.id)
            .map_err(CheckpointControl::capture_failure)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct EmptyCheckpointStore;

    #[cfg(unix)]
    impl crate::composition::CheckpointSink for EmptyCheckpointStore {
        fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn begin_until(&self, _: std::time::Instant) -> Result<std::num::NonZeroU64, CompositionError> {
            Ok(std::num::NonZeroU64::MIN)
        }

        fn put_until(
            &self,
            _: std::num::NonZeroU64,
            _: &str,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn abort_until(&self, _: std::num::NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
            Ok(())
        }

        fn commit_until(
            &self,
            _: std::num::NonZeroU64,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }
    }

    #[cfg(unix)]
    impl crate::composition::CheckpointSource for EmptyCheckpointStore {
        fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }
    }

    #[test]
    fn native_run_status_survives_the_engine_boundary() {
        assert_eq!(native_run_failure(13), EngineError::NativeRunFailed(13));
    }

    #[cfg(unix)]
    #[test]
    fn dropped_recovery_admission_releases_scope_and_rejects_stale_id() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let first = server
            .begin_recovery(11, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        {
            let _admission = RecoveryAdmission {
                server: Arc::clone(&server),
                id: first,
            };
        }
        let second = server
            .begin_recovery(12, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            server.abort_recovery(first),
            Err(super::super::checkpoint::CaptureFailure::Busy)
        );
        server.abort_recovery(second).unwrap();
    }
}
