#![allow(unsafe_code)]

use super::checkpoint_broker::Server;
use crate::composition::{CheckpointSink, CheckpointSource};
use crate::engine::EngineError;
use crate::ffi::checkpoint::{Broker, Trigger};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) struct CheckpointControl {
    server: Arc<Server>,
    trigger: Trigger,
    acceptor: Option<std::thread::JoinHandle<()>>,
}

impl CheckpointControl {
    pub(super) fn start(
        sink: Arc<dyn CheckpointSink>,
        source: Arc<dyn CheckpointSource>,
        broker: Broker,
    ) -> Result<Self, EngineError> {
        let trigger = Trigger::create().map_err(|_| EngineError::LaunchFailed)?;
        let server = Arc::new(Server::new(sink, source));
        let acceptor = Server::start(&server, broker);
        Ok(Self {
            server,
            trigger,
            acceptor: Some(acceptor),
        })
    }

    pub(super) const fn trigger_descriptor(&self) -> i32 {
        self.trigger.descriptor()
    }

    pub(super) fn capture(&self, worker: u32, signal: i32) -> Result<(), EngineError> {
        self.trigger.bump();
        if interrupt_processes(worker, signal) == 0 {
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
                hl_log::hl_error!(
                    hl_log::tag::CHECKPOINT,
                    "retained C checkpoint timed out: broker_connections={} failure={:?}",
                    self.server.connections(),
                    self.server.failure()
                );
                return Err(EngineError::WaitFailed);
            }
            if Instant::now() >= next_interrupt {
                let _ = interrupt_processes(worker, signal);
                next_interrupt = Instant::now() + Duration::from_millis(100);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for CheckpointControl {
    fn drop(&mut self) {
        self.server.stop();
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

#[cfg(target_os = "linux")]
fn interrupt_processes(worker: u32, signal: i32) -> usize {
    let mut pending = vec![worker];
    // The Rust worker is the launcher/control process and deliberately does not
    // install the retained engine's reserved signal handler. Only its guest
    // descendants are valid targets.
    let mut descendants = Vec::new();
    while let Some(parent) = pending.pop() {
        let children = std::fs::read_to_string(format!("/proc/{parent}/task/{parent}/children")).unwrap_or_default();
        for child in children
            .split_whitespace()
            .filter_map(|value| value.parse::<u32>().ok())
        {
            pending.push(child);
            descendants.push(child);
        }
    }
    descendants
        .into_iter()
        // Process-directed delivery lets the kernel select an unblocked executor
        // thread, matching the standalone engine's checkpoint interrupt.
        // SAFETY: every id came from the live worker descendant inventory.
        .filter(|pid| unsafe { libc::kill(*pid as i32, signal) == 0 })
        .count()
}

#[cfg(target_os = "macos")]
fn interrupt_processes(worker: u32, signal: i32) -> usize {
    let _ = (worker, signal);
    0
}
