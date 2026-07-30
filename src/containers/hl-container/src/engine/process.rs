//! One running guest process: its native machine handle, log drain, terminal, and the engine domain
//! whose lifetime it may own.

use crate::{service::Running, Error, ExitStatus, LogChunk, Result, Signal};
use async_trait::async_trait;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

pub(super) struct Process {
    pub(super) id: u64,
    pub(super) child: Mutex<Option<hl_engine::Machine>>,
    pub(super) logs: StdMutex<Option<tokio::sync::mpsc::UnboundedReceiver<LogChunk>>>,
    pub(super) terminal: Option<Arc<StdMutex<hl_engine::Terminal>>>,
    pub(super) domain: hl_engine::Domain,
    pub(super) domain_owner: bool,
    pub(super) checkpointable: bool,
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
        loop {
            let mut child = self.child.lock().await;
            let Some(process) = child.as_mut() else {
                return Err(Error::Runtime("process result was already consumed".into()));
            };
            if let Some(exit) = process
                .try_wait()
                .map_err(|error| Error::Runtime(error.to_string()))?
            {
                // `try_wait` consumed the native result. Drop the machine handle before domain cleanup so a
                // cleanup failure cannot leave a consumed process stored as if it were still live.
                child.take();
                if self.domain_owner {
                    self.terminate_domain()?;
                }
                return Ok(match exit {
                    hl_engine::Exit::Code(code) => ExitStatus::Code(code),
                    hl_engine::Exit::Signal(signal) => ExitStatus::Signal(signal),
                    hl_engine::Exit::Fault { status, detail } => {
                        ExitStatus::Fault { status, detail }
                    }
                });
            }
            drop(child);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    async fn signal(&self, signal: Signal) -> Result<()> {
        if signal == Signal::Kill {
            let stopped = if let Some(child) = self.child.lock().await.as_mut() {
                match child.force_stop() {
                    Ok(()) => Ok(()),
                    Err(hl_engine::Error::Engine { status, .. }) if status == nix::libc::ESRCH => {
                        Ok(())
                    }
                    Err(error) => Err(Error::Runtime(error.to_string())),
                }
            } else {
                Ok(())
            };
            if self.domain_owner {
                let terminated = self.terminate_domain();
                return match (stopped, terminated) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                    (Err(process), Err(domain)) => Err(Error::Runtime(format!(
                        "{process}; engine domain cleanup also failed: {domain}"
                    ))),
                };
            }
            return stopped;
        }
        let raw = match signal {
            Signal::Terminate => nix::sys::signal::Signal::SIGTERM,
            Signal::Interrupt => nix::sys::signal::Signal::SIGINT,
            Signal::Quit => nix::sys::signal::Signal::SIGQUIT,
            Signal::Hangup => nix::sys::signal::Signal::SIGHUP,
            Signal::User1 => nix::sys::signal::Signal::SIGUSR1,
            Signal::User2 => nix::sys::signal::Signal::SIGUSR2,
            Signal::Kill => unreachable!(),
        };
        self.send(raw)
    }

    async fn pause(&self) -> Result<()> {
        self.send(nix::sys::signal::Signal::SIGSTOP)
    }

    async fn resume(&self) -> Result<()> {
        self.send(nix::sys::signal::Signal::SIGCONT)
    }

    async fn checkpoint(&self, timeout: std::time::Duration) -> Result<()> {
        self.child
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| Error::Runtime("process result was already consumed".into()))?
            .checkpoint_into_store(timeout)
            .map_err(|error| Error::Runtime(error.to_string()))
    }

    async fn resize(&self, size: crate::Size) -> Result<()> {
        let terminal = self
            .terminal
            .as_ref()
            .ok_or_else(|| Error::NoTerminal(self.id.to_string()))?;
        let size = hl_engine::Size::new(size.rows(), size.columns())
            .map_err(|error| Error::Runtime(error.to_string()))?;
        terminal
            .lock()
            .map_err(|_| Error::Runtime("terminal lock is poisoned".into()))?
            .resize(size)
            .map_err(|error| Error::Runtime(error.to_string()))
    }

    fn take_logs(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<LogChunk>> {
        self.logs.lock().ok()?.take()
    }
}

impl Process {
    fn terminate_domain(&self) -> Result<()> {
        match self.domain.terminate() {
            Ok(()) => Ok(()),
            Err(hl_engine::Error::Engine { status, .. }) if status == nix::libc::ESRCH => Ok(()),
            Err(error) => Err(Error::Runtime(error.to_string())),
        }
    }

    fn send(&self, signal: nix::sys::signal::Signal) -> Result<()> {
        Self::send_id(self.id, signal)
    }

    pub(super) fn send_id(id: u64, signal: nix::sys::signal::Signal) -> Result<()> {
        let pid = i32::try_from(id)
            .map_err(|_| Error::Runtime("process id exceeds host range".into()))?;
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(error) => Err(Error::Runtime(error.to_string())),
        }
    }
}
