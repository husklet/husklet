#![allow(unsafe_code)]

#[cfg(unix)]
use crate::composition::{CheckpointSink, CheckpointSource};
use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use std::ffi::CString;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
#[path = "execution_checkpoint.rs"]
mod checkpoint;
#[cfg(all(unix, test))]
pub(crate) use checkpoint::await_capture_completion;
#[cfg(unix)]
use checkpoint::*;

#[cfg(unix)]
use super::checkpoint::Server;

#[cfg(unix)]
use super::terminal::NativeOutputBridge;
#[cfg(unix)]
use super::terminal::{InputDiscipline, NativeTerminalBridge};

const REQUEST_INTERRUPT: u32 = 1;
const REQUEST_FORCE_STOP: u32 = 2;
const REQUEST_SIGNAL: u32 = 3;
#[cfg(unix)]
const REQUEST_CHECKPOINT: u32 = 4;

fn checkpoint_sandbox_refusal(options: &crate::options::Options) -> Option<EngineError> {
    options
        .get_bytes("HL_UNTRUSTED")
        .map(|_| EngineError::CheckpointUnsupportedUnderSandbox)
}

fn native_run_failure(status: i32) -> EngineError {
    EngineError::NativeRunFailed(status)
}

pub(crate) struct ProductionMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launcher::plan::RuntimePlan,
    #[cfg(unix)]
    terminal: Option<NativeTerminalBridge>,
    #[cfg(unix)]
    output: Option<NativeOutputBridge>,
    #[cfg(unix)]
    checkpoint: Option<CheckpointControl>,
    /// Set instead of `checkpoint` when this machine joins a domain freeze it does
    /// not coordinate. A member has no `Server`, so it has no channel to publish an
    /// image of its own on; its guest processes commit into the coordinator's store.
    #[cfg(unix)]
    member: Option<crate::composition::CheckpointChannel>,
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
            .map(|terminal| NativeTerminalBridge::attach(terminal, InputDiscipline::Linux))
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
        let member = request.services.checkpoint_channel.clone();
        #[cfg(unix)]
        let checkpoint = match (
            request.services.checkpoint_sink.clone(),
            request.services.checkpoint_source.clone(),
        ) {
            (Some(sink), Some(source)) => Some(CheckpointControl::start(
                sink,
                source,
                request.isa,
                request
                    .plan
                    .options
                    .get_bytes("HL_CHECKPOINT_PHASE_LEDGER")
                    .and_then(|_| request.plan.options.get("HL_DIAGNOSTIC_PORT"))
                    .and_then(|value| value.parse().ok()),
                request
                    .plan
                    .options
                    .get_bytes("HL_CHECKPOINT_PHASE_CLOCK_FAIL")
                    .is_some(),
            )?),
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
            #[cfg(unix)]
            member,
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
        #[cfg(unix)]
        self.plan.refuse_unownable_root()?;
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
        // Name the coordinator on the launch boundary. Only a machine holding a CheckpointControl can be
        // sent REQUEST_CHECKPOINT, so this is exactly "the embedder will ask THIS engine to capture"; a
        // domain member carries a channel and no Server. The engine's election reads it instead of asking
        // whether it is the top of a launch, which every exec session also is.
        #[cfg(unix)]
        if self.checkpoint.is_some() {
            options.push((
                CString::new("HL_CHECKPOINT_COORDINATOR").expect("literal"),
                CString::new("1").expect("literal"),
            ));
        }
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
        let engine = unsafe { hl_native::Engine::create(config) }.map_err(|error| match error {
            hl_native::Error::Load(kind) => EngineError::NativeLoadFailed(kind),
            hl_native::Error::Status(status) => EngineError::NativeCreateFailed(status),
        })?;
        #[cfg(unix)]
        let mut engine = engine;
        #[cfg(unix)]
        if let Some(transport) = self
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.transport.as_ref())
            .or_else(|| self.member.as_ref().map(|channel| channel.0.as_ref()))
        {
            engine
                .configure_checkpoint(transport)
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
            // A refused recovery admission must name itself: the restore driver downstream is the only
            // other place that reports, and it never runs when admission refuses.
            Some(
                checkpoint
                    .begin_recovery(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)
                    .inspect_err(|error| eprintln!("[restore] refuse: recovery admission rejected: {error:?}"))?,
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
            run_with_recovery(recovery, || engine.run(&pointers).map_err(native_run_failure))
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

    #[cfg(unix)]
    fn checkpoint_channel(&self) -> Option<crate::composition::CheckpointChannel> {
        self.checkpoint
            .as_ref()
            .map(|checkpoint| crate::composition::CheckpointChannel(Arc::clone(&checkpoint.transport)))
    }

    fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        self.current().ok().and_then(|engine| engine.guest_pid())
    }

    #[cfg(unix)]
    fn restored_member(&self, guest_pid: std::num::NonZeroI32) -> Option<crate::runtime::RestoredMember> {
        self.checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.server.restored_member(guest_pid))
            .map(crate::runtime::RestoredMember::new)
    }

    #[cfg(unix)]
    fn provide_member_terminal(
        &self,
        guest_pid: std::num::NonZeroI32,
        terminal: std::os::fd::OwnedFd,
    ) -> Result<(), EngineError> {
        let checkpoint = self.checkpoint.as_ref().ok_or(EngineError::Unsupported)?;
        checkpoint
            .server
            .register_member_terminal(guest_pid, terminal)
            .map_err(|reason| {
                hl_log::hl_error!(hl_log::tag::CHECKPOINT, "member terminal registration failed: {reason}");
                EngineError::Unsupported
            })
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        if let Some(refusal) = checkpoint_sandbox_refusal(&self.plan.options) {
            return Err(refusal);
        }
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
        if let Some(refusal) = checkpoint_sandbox_refusal(&self.plan.options) {
            return Err(refusal);
        }
        #[cfg(unix)]
        if let Some(checkpoint) = &self.checkpoint {
            let engine = self.current()?;
            return checkpoint.capture(engine.as_ref(), self.isa, deadline);
        }
        Err(EngineError::Unsupported)
    }
}

/// Classify a launch that the checkpoint engine cannot capture under.
///
/// `HL_UNTRUSTED` forks the sentry and routes every host-authority syscall through it, so the
/// worker process that would dump itself does not own the descriptors, sockets or pipes the guest
/// sees; `ckpt_dump_self_locked` refuses on that gate. Capturing under the sentry requires the
/// sentry to participate in capture and restore -- exporting its descriptor table, open-file
/// descriptions and connection state across the control ring -- which is not implemented.
///
/// Reporting it here rather than as a bare native failure keeps the refusal on the launch-policy
/// boundary that owns the option, and makes it permanent so a preflight does not poll for it.
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
            // Recovery refuses a generation carrying no manifest, so a store that
            // admits recovery must present a finalized one.
            Ok(vec![String::from("MANIFEST")])
        }
    }

    /// A staged generation never becomes finalized while this launch is waiting,
    /// so the checkpoint preflight must surface the refusal instead of polling
    /// until its deadline.
    ///
    /// `#[cfg(unix)]` because both names in the body are: `CheckpointControl` is
    /// declared under one in this file, and `super::super::checkpoint` under one in
    /// `runtime/api.rs`. The gate belongs on the test rather than on the module,
    /// because the module is a checkpoint coordinator that passes descriptors over an
    /// `AF_UNIX` socket with `SCM_RIGHTS` -- widening it to Windows is a port, not a
    /// `cfg` edit. Everything else in this `mod tests` that names either already
    /// carries the same gate; this one item did not, and nothing built the
    /// configuration that would have said so.
    #[cfg(unix)]
    #[test]
    fn an_unfinalized_generation_is_a_permanent_recovery_refusal() {
        assert_eq!(
            CheckpointControl::capture_failure(super::super::checkpoint::CaptureFailure::Unfinalized),
            EngineError::CheckpointGenerationUnfinalized
        );
        assert!(EngineError::CheckpointGenerationUnfinalized.is_permanent_refusal());
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
                state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
                phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
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

    #[cfg(unix)]
    #[test]
    fn pre_channel_run_failure_aborts_recovery_without_waiting_for_deadline() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(21, std::time::Instant::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let started = std::time::Instant::now();
        assert_eq!(
            run_with_recovery(&admission, || Err::<(), _>(EngineError::NativeRunFailed(7))),
            Err(EngineError::NativeRunFailed(7))
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        assert_eq!(admission.wait(), Err(EngineError::Busy));
        let retry = server
            .begin_recovery(22, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        server.abort_recovery(retry).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dropping_consumed_admission_cannot_abort_reused_generation() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(23, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        admission.abort().unwrap();
        let _ = admission.wait();

        let reused = server
            .begin_recovery(23, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        drop(admission);
        server
            .fail_recovery(reused, super::super::checkpoint::CaptureFailure::Deadline)
            .expect("a consumed admission must not abort the reused generation");
        assert_eq!(
            server.wait_recovery(reused),
            Err(super::super::checkpoint::CaptureFailure::Deadline)
        );
    }

    #[cfg(unix)]
    #[test]
    fn poisoned_wait_is_aborted_by_admission_drop_and_allows_retry() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(24, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let poison = Arc::clone(&server);
        let _ = std::thread::spawn(move || poison.poison_coordination()).join();

        // A poisoned capture ledger is not a launch failure. Naming it as one is what put
        // "LaunchFailed" in front of a desktop user whose workspace had launched fine.
        assert_eq!(admission.wait(), Err(EngineError::CapturePoisoned));
        drop(admission);
        let retry = server
            .begin_recovery(25, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .expect("dropping a poisoned admission must release its recovery transaction");
        server.abort_recovery(retry).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn poisoned_unwaited_admission_drop_allows_retry() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(26, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let poison = Arc::clone(&server);
        let _ = std::thread::spawn(move || poison.poison_coordination()).join();

        drop(admission);
        let retry = server
            .begin_recovery(27, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .expect("dropping an unwaited poisoned admission must release its recovery transaction");
        server.abort_recovery(retry).unwrap();
    }
}

#[cfg(test)]
mod sandbox_refusal_tests {
    use super::*;

    /// `Sandbox::SentryOnly` is the container default, so this is the ordinary launch. A checkpoint
    /// of it must refuse with a cause the product can show, not with a bare native failure, and the
    /// refusal must be permanent so the checkpoint preflight reports it instead of polling for 30s.
    #[test]
    fn a_sentry_launch_is_refused_permanently_with_its_own_cause() {
        let mut options = crate::options::Options::default();
        options.set("HL_UNTRUSTED", "1", true).unwrap();
        assert_eq!(
            checkpoint_sandbox_refusal(&options),
            Some(EngineError::CheckpointUnsupportedUnderSandbox)
        );
        assert!(EngineError::CheckpointUnsupportedUnderSandbox.is_permanent_refusal());
    }

    #[test]
    fn a_launch_without_the_sentry_is_not_refused_by_policy() {
        assert_eq!(checkpoint_sandbox_refusal(&crate::options::Options::default()), None);
    }
}
