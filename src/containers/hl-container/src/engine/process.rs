//! One running container process backed by the integrated Rust runtime.

use crate::{Error, ExitStatus, Result, Signal, service::Running};
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

static NEXT_PROCESS: AtomicU64 = AtomicU64::new(1);

fn wait_thread<T, F>(name: String, operation: F) -> std::io::Result<tokio::sync::oneshot::Receiver<T>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (result, wait) = tokio::sync::oneshot::channel();
    std::thread::Builder::new().name(name).spawn(move || {
        let _ = result.send(operation());
    })?;
    Ok(wait)
}

pub(super) struct Process {
    pub(super) id: u64,
    pub(super) child: Mutex<Option<Arc<hl_engine::runtime::Engine>>>,
    pub(super) logs: Mutex<Option<crate::service::LogReceiver>>,
    pub(super) domain: hl_engine::Domain,
    pub(super) checkpointable: bool,
}

impl Process {
    pub(super) fn next_id() -> u64 {
        NEXT_PROCESS.fetch_add(1, Ordering::Relaxed)
    }

    fn status(exit: hl_engine::engine::EngineExit) -> ExitStatus {
        match exit.kind {
            hl_engine::engine::ExitKind::Code => ExitStatus::Code(exit.guest_status),
            hl_engine::engine::ExitKind::Signal => ExitStatus::Signal(exit.guest_status),
            hl_engine::engine::ExitKind::Fault | hl_engine::engine::ExitKind::EngineError => ExitStatus::Fault {
                status: exit.guest_status,
                detail: exit.detail,
                reason: exit
                    .fault
                    .map_or(crate::FaultCause::Unknown, |fault| match fault.reason {
                        hl_engine::engine::FaultReason::Fetch => crate::FaultCause::Fetch,
                        hl_engine::engine::FaultReason::Memory => crate::FaultCause::Memory,
                        hl_engine::engine::FaultReason::Decode => crate::FaultCause::Decode,
                        hl_engine::engine::FaultReason::Unsupported => crate::FaultCause::Unsupported,
                        hl_engine::engine::FaultReason::Frozen => crate::FaultCause::Frozen,
                        hl_engine::engine::FaultReason::CacheEpoch => crate::FaultCause::CacheEpoch,
                        hl_engine::engine::FaultReason::Protocol => crate::FaultCause::Protocol,
                        hl_engine::engine::FaultReason::NativeFatal => crate::FaultCause::NativeFatal,
                    }),
            },
        }
    }

    fn engine(&self) -> Result<Arc<hl_engine::runtime::Engine>> {
        self.child
            .lock()
            .map_err(|_| Error::Runtime("engine process lock is poisoned".into()))?
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::Runtime("process result was already consumed".into()))
    }

    /// `None` once the guest has been reaped, which callers that tolerate a terminal guest use
    /// to stay a no-op instead of reporting a stop failure.
    fn live(&self) -> Result<Option<Arc<hl_engine::runtime::Engine>>> {
        Ok(self
            .child
            .lock()
            .map_err(|_| Error::Runtime("engine process lock is poisoned".into()))?
            .as_ref()
            .cloned())
    }

    /// A guest that already reached a terminal state is the outcome a stop asked for, so it
    /// succeeds; every other engine failure, including a live guest that refused, is reported.
    fn stopped(result: std::result::Result<(), hl_engine::engine::EngineError>) -> Result<()> {
        match result {
            Ok(()) | Err(hl_engine::engine::EngineError::Exited) => Ok(()),
            Err(error) => Err(Error::Runtime(format!("engine stop: {error:?}"))),
        }
    }

    fn request(signal: Signal) -> hl_engine::engine::StopRequest {
        match signal {
            Signal::KILL => hl_engine::engine::StopRequest::Force,
            Signal::INTERRUPT => hl_engine::engine::StopRequest::Interrupt,
            other => hl_engine::engine::StopRequest::Signal(i32::from(other.get())),
        }
    }
}

#[async_trait]
impl Running for Process {
    fn id(&self) -> u64 {
        self.id
    }

    fn domain(&self) -> hl_engine::Domain {
        self.domain
    }

    fn checkpointable(&self) -> bool {
        self.checkpointable
    }

    async fn wait(self: Arc<Self>) -> Result<ExitStatus> {
        let engine = self.engine()?;
        let wait = wait_thread(format!("hl-engine-wait-{}", self.id), move || engine.wait())
            .map_err(|error| Error::Runtime(format!("engine wait thread: {error}")))?;
        let exit = wait
            .await
            .map_err(|_| Error::Runtime("engine wait thread ended without a result".into()))?
            .map_err(|error| Error::Runtime(format!("engine wait: {error:?}")))?;
        self.child
            .lock()
            .map_err(|_| Error::Runtime("engine process lock is poisoned".into()))?
            .take();
        Ok(Self::status(exit))
    }

    async fn signal(&self, signal: Signal) -> Result<()> {
        let Some(engine) = self.live()? else { return Ok(()) };
        Self::stopped(engine.stop(Self::request(signal)))
    }

    async fn pause(&self) -> Result<()> {
        self.engine()?
            .stop(hl_engine::engine::StopRequest::Signal(19))
            .map_err(|error| Error::Runtime(format!("engine pause: {error:?}")))
    }

    async fn resume(&self) -> Result<()> {
        self.engine()?
            .stop(hl_engine::engine::StopRequest::Signal(18))
            .map_err(|error| Error::Runtime(format!("engine resume: {error:?}")))
    }

    async fn checkpoint(&self, timeout: std::time::Duration) -> Result<()> {
        let engine = self.engine()?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match engine.checkpoint_supported() {
                Ok(()) => {
                    return engine
                        .capture_checkpoint_until(deadline.into_std())
                        .map_err(|error| Error::Runtime(format!("engine checkpoint: {error:?}")));
                }
                // A permanent refusal (an unsupported sandbox policy) never becomes supported by
                // waiting; polling it to the deadline would report a 30s stall instead of a cause.
                Err(error) if error.is_permanent_refusal() => {
                    return Err(Error::Runtime(format!("engine checkpoint unsupported: {error:?}")));
                }
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                Err(error) => {
                    return Err(Error::Runtime(format!("engine checkpoint preflight: {error:?}")));
                }
            }
        }
    }

    async fn resize(&self, size: crate::Size) -> Result<()> {
        self.engine()?
            .resize_terminal(size.rows(), size.columns())
            .map_err(|error| match error {
                hl_engine::engine::EngineError::Unsupported => Error::NoTerminal(self.id.to_string()),
                _ => Error::Runtime(format!("terminal resize: {error:?}")),
            })
    }

    fn take_logs(&self) -> Option<crate::service::LogReceiver> {
        self.logs.lock().ok()?.take()
    }
}

#[cfg(test)]
mod runtime_drop_tests {
    use super::wait_thread;

    #[test]
    fn indefinite_native_wait_does_not_block_runtime_drop() {
        const CHILD: &str = "HL_TEST_INDEFINITE_WAIT_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            runtime.block_on(async {
                let wait = wait_thread("hl-test-indefinite-native-wait".into(), || {
                    loop {
                        std::thread::park();
                    }
                })
                .unwrap();
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(10), wait)
                        .await
                        .is_err()
                );
            });
            drop(runtime);
            return;
        }

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine::process::runtime_drop_tests::indefinite_native_wait_does_not_block_runtime_drop",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "wait subprocess failed: {status}");
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("indefinite native wait blocked Tokio runtime drop");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Process;
    use crate::service::Running as _;
    use crate::{ExitStatus, FaultCause, Signal};
    use std::sync::{Arc, Mutex};

    fn reaped() -> Arc<Process> {
        Arc::new(Process {
            id: 1,
            child: Mutex::new(None),
            logs: Mutex::new(None),
            domain: hl_engine::Domain::new().unwrap(),
            checkpointable: false,
        })
    }

    /// Docker answers `stop` on an exited container with 304, not an error, so a teardown that
    /// races the guest's own exit must not report a stop failure.
    #[tokio::test]
    async fn stopping_a_reaped_guest_succeeds() {
        for signal in [Signal::TERMINATE, Signal::KILL, Signal::INTERRUPT] {
            reaped().signal(signal).await.unwrap();
        }
    }

    /// Every signal outside the two terminal shortcuts reaches the engine as its own number.
    #[test]
    fn every_signal_number_reaches_the_engine() {
        use hl_engine::engine::StopRequest;
        assert_eq!(Process::request(Signal::KILL), StopRequest::Force);
        assert_eq!(Process::request(Signal::INTERRUPT), StopRequest::Interrupt);
        for number in 1..=Signal::MAXIMUM {
            let signal = Signal::new(number).unwrap();
            if signal == Signal::KILL || signal == Signal::INTERRUPT {
                continue;
            }
            assert_eq!(Process::request(signal), StopRequest::Signal(i32::from(number)));
        }
    }

    /// A stop that cannot reach a live guest still has to fail; only the terminal answer is
    /// absorbed.
    #[test]
    fn only_a_terminal_guest_makes_a_stop_a_no_op() {
        use hl_engine::engine::EngineError;
        Process::stopped(Err(EngineError::Exited)).unwrap();
        Process::stopped(Ok(())).unwrap();
        for error in [
            EngineError::Busy,
            EngineError::StopFailed,
            EngineError::Destroyed,
            EngineError::Synchronization,
        ] {
            assert!(Process::stopped(Err(error)).is_err(), "{error:?} must not be absorbed");
        }
    }

    /// `pause` and `unpause` on a container that is not running are conflicts in Docker, so the
    /// terminal-state tolerance must not spread beyond stop.
    #[tokio::test]
    async fn pausing_a_reaped_guest_still_fails() {
        assert!(reaped().pause().await.is_err());
        assert!(reaped().resume().await.is_err());
    }

    fn exit(reason: hl_engine::engine::FaultReason) -> hl_engine::engine::EngineExit {
        hl_engine::engine::EngineExit {
            kind: hl_engine::engine::ExitKind::Fault,
            guest_status: 0,
            detail: 0,
            fault: Some(hl_engine::engine::FaultDiagnostic {
                isa: hl_engine::activation::GuestIsa::Aarch64,
                pc: 0,
                opcode: [0; 15],
                opcode_len: 0,
                reason,
                address: None,
                access: None,
            }),
        }
    }

    /// The reasons that carry no faulting instruction are indistinguishable by status and
    /// detail alone, so a fault that drops its reason cannot be classified at all.
    #[test]
    fn reasonless_faults_stay_distinguishable_after_mapping() {
        for (reason, expected) in [
            (hl_engine::engine::FaultReason::Frozen, FaultCause::Frozen),
            (hl_engine::engine::FaultReason::CacheEpoch, FaultCause::CacheEpoch),
            (hl_engine::engine::FaultReason::Protocol, FaultCause::Protocol),
        ] {
            assert_eq!(
                Process::status(exit(reason)),
                ExitStatus::Fault {
                    status: 0,
                    detail: 0,
                    reason: expected
                }
            );
        }
    }

    #[test]
    fn a_fault_without_a_diagnostic_reports_unknown() {
        assert_eq!(
            Process::status(hl_engine::engine::EngineExit {
                fault: None,
                ..exit(hl_engine::engine::FaultReason::Frozen)
            }),
            ExitStatus::Fault {
                status: 0,
                detail: 0,
                reason: FaultCause::Unknown
            }
        );
    }
}
