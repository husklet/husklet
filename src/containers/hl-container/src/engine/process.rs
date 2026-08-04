//! One running container process backed by the integrated Rust runtime.

use crate::{Error, ExitStatus, Result, Signal, service::Running};
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

static NEXT_PROCESS: AtomicU64 = AtomicU64::new(1);

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

    fn engine(&self) -> Result<Arc<hl_engine::runtime::Engine>> {
        self.child
            .lock()
            .map_err(|_| Error::Runtime("engine process lock is poisoned".into()))?
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::Runtime("process result was already consumed".into()))
    }

    fn request(signal: Signal) -> hl_engine::engine::StopRequest {
        match signal {
            Signal::Kill => hl_engine::engine::StopRequest::Force,
            Signal::Interrupt => hl_engine::engine::StopRequest::Interrupt,
            Signal::Terminate => hl_engine::engine::StopRequest::Signal(15),
            Signal::Quit => hl_engine::engine::StopRequest::Signal(3),
            Signal::Hangup => hl_engine::engine::StopRequest::Signal(1),
            Signal::User1 => hl_engine::engine::StopRequest::Signal(10),
            Signal::User2 => hl_engine::engine::StopRequest::Signal(12),
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
        let exit = tokio::task::spawn_blocking(move || engine.wait())
            .await
            .map_err(|error| Error::Runtime(format!("engine wait task: {error}")))?
            .map_err(|error| Error::Runtime(format!("engine wait: {error:?}")))?;
        self.child
            .lock()
            .map_err(|_| Error::Runtime("engine process lock is poisoned".into()))?
            .take();
        Ok(match exit.kind {
            hl_engine::engine::ExitKind::Code => ExitStatus::Code(exit.guest_status),
            hl_engine::engine::ExitKind::Signal => ExitStatus::Signal(exit.guest_status),
            hl_engine::engine::ExitKind::Fault | hl_engine::engine::ExitKind::EngineError => ExitStatus::Fault {
                status: exit.guest_status,
                detail: exit.detail,
            },
        })
    }

    async fn signal(&self, signal: Signal) -> Result<()> {
        self.engine()?
            .stop(Self::request(signal))
            .map_err(|error| Error::Runtime(format!("engine stop: {error:?}")))
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
                        .capture_checkpoint()
                        .map_err(|error| Error::Runtime(format!("engine checkpoint: {error:?}")));
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

    async fn resize(&self, _size: crate::Size) -> Result<()> {
        Err(Error::NoTerminal(self.id.to_string()))
    }

    fn take_logs(&self) -> Option<crate::service::LogReceiver> {
        self.logs.lock().ok()?.take()
    }
}
