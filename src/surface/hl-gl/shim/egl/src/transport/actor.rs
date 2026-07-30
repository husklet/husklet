//! FIFO ownership boundary for the guest-to-host GPU transport.
//!
//! A [`Sequencer`] assigns an order while holding the same lock that enqueues the work. The actor thread is
//! the only owner of the remote sink, so every [`Plan`] runs as one non-interleaved compound operation.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use hl_gpu::{GpuError, RemoteCommandSink};

type Operation<T> = Box<dyn FnOnce(&mut RemoteCommandSink) -> hl_gpu::Result<T> + Send + 'static>;
type Job = Box<dyn FnOnce(&mut RemoteCommandSink) -> bool + Send + 'static>;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(100);

#[must_use]
pub(crate) struct Plan<T> {
    operation: Operation<T>,
}

impl<T> Plan<T> {
    pub(crate) fn new(
        operation: impl FnOnce(&mut RemoteCommandSink) -> hl_gpu::Result<T> + Send + 'static,
    ) -> Self {
        Self {
            operation: Box::new(operation),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Serial(u64);

impl Serial {
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

#[must_use = "transport failures must be observed"]
pub(crate) struct Ticket<T> {
    serial: Serial,
    result: Receiver<hl_gpu::Result<T>>,
}

impl<T> Ticket<T> {
    pub(crate) fn serial(&self) -> Serial {
        self.serial
    }

    pub(crate) fn wait(self) -> hl_gpu::Result<T> {
        self.result.recv().unwrap_or_else(|_| {
            Err(GpuError::Decode(
                "GPU transport actor stopped before completing a plan".into(),
            ))
        })
    }

    pub(crate) fn wait_for(
        self,
        timeout: Duration,
    ) -> Result<hl_gpu::Result<T>, mpsc::RecvTimeoutError> {
        self.result.recv_timeout(timeout)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubmitError {
    ActorFailed,
    Closed,
    SerialExhausted,
}

impl fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActorFailed => formatter.write_str("GPU transport actor failed"),
            Self::Closed => formatter.write_str("GPU transport actor is closed"),
            Self::SerialExhausted => formatter.write_str("GPU transport serials are exhausted"),
        }
    }
}

impl std::error::Error for SubmitError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Shutdown {
    /// The actor acknowledged shutdown and its thread was joined.
    Stopped,
    /// The actor could not be joined without blocking teardown and was detached.
    ///
    /// The detached thread owns its sink, queued operations, closures, and result senders. Operations must
    /// own all of their data and never borrow display [`State`](crate::State) or caller-managed storage.
    /// Callers cannot infer that accepted operations completed.
    Detached,
}

enum Message {
    Execute(Job),
    Shutdown(Sender<()>),
}

struct Sequence {
    next: Option<u64>,
    sender: Option<Sender<Message>>,
}

struct Actor {
    actor_thread: ThreadId,
    failed: Arc<AtomicBool>,
    sequence: Mutex<Sequence>,
    shutdown: Mutex<()>,
    terminal: Mutex<Option<Shutdown>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Actor {
    fn shutdown(&self) -> Shutdown {
        let _shutdown = self
            .shutdown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(terminal) = *terminal {
            return terminal;
        }
        drop(terminal);
        let (done_tx, done_rx) = mpsc::channel();
        let sender = self
            .sequence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sender
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(Message::Shutdown(done_tx));
        }
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(thread) = thread else {
            return self.finish(Shutdown::Stopped);
        };
        if thread::current().id() == self.actor_thread {
            drop(thread);
            return self.finish(Shutdown::Detached);
        }
        match done_rx.recv_timeout(SHUTDOWN_TIMEOUT) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if thread.join().is_err() {
                    self.failed.store(true, Ordering::Release);
                }
                self.finish(Shutdown::Stopped)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Rust cannot cancel a thread blocked in host I/O. Detach it after rejecting new work so
                // EGL teardown remains bounded; dropping its receiver eventually releases queued plans.
                drop(thread);
                self.finish(Shutdown::Detached)
            }
        }
    }

    fn finish(&self, terminal: Shutdown) -> Shutdown {
        *self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(terminal);
        terminal
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Clone)]
pub(crate) struct Sequencer {
    actor: Arc<Actor>,
}

impl Sequencer {
    pub(crate) fn spawn(sink: RemoteCommandSink) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let failed = Arc::new(AtomicBool::new(false));
        let actor_failed = Arc::clone(&failed);
        let thread = thread::Builder::new()
            .name("hl-gl-transport".into())
            .spawn(move || run(sink, receiver, actor_failed))?;
        let actor_thread = thread.thread().id();
        Ok(Self {
            actor: Arc::new(Actor {
                actor_thread,
                failed,
                sequence: Mutex::new(Sequence {
                    next: Some(1),
                    sender: Some(sender),
                }),
                shutdown: Mutex::new(()),
                terminal: Mutex::new(None),
                thread: Mutex::new(Some(thread)),
            }),
        })
    }

    pub(crate) fn submit<T: Send + 'static>(
        &self,
        plan: Plan<T>,
    ) -> Result<Ticket<T>, SubmitError> {
        let mut sequence = self
            .actor
            .sequence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.actor.failed.load(Ordering::Acquire) {
            return Err(SubmitError::ActorFailed);
        }
        let sender = sequence.sender.as_ref().ok_or(SubmitError::Closed)?;
        let serial = Serial(sequence.next.ok_or(SubmitError::SerialExhausted)?);
        let next = sequence.next.and_then(|candidate| candidate.checked_add(1));
        let (result_tx, result_rx) = mpsc::channel();
        let failed = Arc::clone(&self.actor.failed);
        let job =
            Box::new(
                move |sink: &mut RemoteCommandSink| match execute(plan.operation, sink) {
                    Ok(result) => {
                        let _ = result_tx.send(result);
                        true
                    }
                    Err(_) => {
                        // Publish terminal actor state before waking the failed ticket. A caller that waits
                        // and immediately submits again must observe ActorFailed rather than racing `run`.
                        failed.store(true, Ordering::Release);
                        let _ = result_tx
                            .send(Err(GpuError::Decode("GPU transport plan panicked".into())));
                        false
                    }
                },
            );
        sender
            .send(Message::Execute(job))
            .map_err(|_| SubmitError::Closed)?;
        sequence.next = next;
        Ok(Ticket {
            serial,
            result: result_rx,
        })
    }

    /// Work already accepted remains ordered. It is drained only when the actor acknowledges within the
    /// shutdown bound; a thread blocked in host I/O is detached because Rust cannot cancel it.
    pub(crate) fn shutdown(&self) -> Shutdown {
        self.actor.shutdown()
    }

    pub(crate) fn fail(&self) -> Shutdown {
        self.actor.failed.store(true, Ordering::Release);
        self.actor.shutdown()
    }
}

#[cfg(panic = "unwind")]
fn execute<T>(
    operation: Operation<T>,
    sink: &mut RemoteCommandSink,
) -> Result<hl_gpu::Result<T>, ()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(sink))).map_err(|_| ())
}

#[cfg(panic = "abort")]
fn execute<T>(
    operation: Operation<T>,
    sink: &mut RemoteCommandSink,
) -> Result<hl_gpu::Result<T>, ()> {
    Ok(operation(sink))
}

fn run(mut sink: RemoteCommandSink, receiver: Receiver<Message>, failed: Arc<AtomicBool>) {
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Execute(job) => {
                if !job(&mut sink) {
                    failed.store(true, Ordering::Release);
                    break;
                }
            }
            Message::Shutdown(done) => {
                let _ = done.send(());
                break;
            }
        }
    }
}

#[cfg(all(test, panic = "unwind"))]
#[path = "actor/panic.rs"]
mod panic;

#[cfg(test)]
#[path = "actor/serial.rs"]
mod serial;

#[cfg(test)]
#[path = "actor/tests.rs"]
mod tests;
