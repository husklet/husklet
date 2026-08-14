use hl_client::api::{Size, TerminalInput};
use hl_client::Client;
use hl_ws_term::PtyBackend;
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::os::unix::io::RawFd;
use std::sync::{Arc, Mutex};

use super::PaneExecution;

pub(super) struct Shell;

impl Shell {
    pub(super) fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(super) struct ExecPty {
    pub(super) runtime: tokio::runtime::Runtime,
    pub(super) client: Client,
    pub(super) execution: String,
    pub(super) input: TerminalInput,
    pub(super) output: Output,
    pub(super) exited: Arc<Mutex<Option<i32>>>,
    pub(super) pane: Option<PaneExecution>,
}

const CLEANUP_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

async fn cleanup_with<Signal, SignalFuture, Wait, WaitFuture, Remove, RemoveFuture, Error>(
    live: bool,
    timeout: std::time::Duration,
    signal: Signal,
    wait: Wait,
    remove: Remove,
) -> Vec<String>
where
    Signal: FnOnce() -> SignalFuture,
    SignalFuture: Future<Output = Result<(), Error>>,
    Wait: FnOnce() -> WaitFuture,
    WaitFuture: Future<Output = Result<(), Error>>,
    Remove: FnOnce() -> RemoveFuture,
    RemoveFuture: Future<Output = Result<(), Error>>,
    Error: std::fmt::Display,
{
    let mut failures = Vec::new();
    if live {
        match tokio::time::timeout(timeout, signal()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(format!("signal: {error}")),
            Err(_) => failures.push("signal: timed out".into()),
        }
        match tokio::time::timeout(timeout, wait()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(format!("wait: {error}")),
            Err(_) => failures.push("wait: timed out".into()),
        }
    }
    match tokio::time::timeout(timeout, remove()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => failures.push(format!("remove: {error}")),
        Err(_) => failures.push("remove: timed out".into()),
    }
    failures
}

pub(super) struct Output {
    receiver: std::sync::mpsc::Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
    closed: bool,
}

impl Output {
    pub(super) fn new(receiver: std::sync::mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            pending: VecDeque::new(),
            closed: false,
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> usize {
        let mut read = 0;
        while read < buffer.len() {
            if let Some(byte) = self.pending.pop_front() {
                buffer[read] = byte;
                read += 1;
                continue;
            }
            match self.receiver.try_recv() {
                Ok(bytes) => self.pending.extend(bytes),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    break;
                }
            }
        }
        read
    }

    fn finished(&self) -> bool {
        self.closed && self.pending.is_empty()
    }
}

impl PtyBackend for ExecPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.runtime.block_on(self.input.write(bytes)).map_err(io::Error::other)
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        Ok(self.output.read(buffer))
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        if let Ok(size) = Size::new(rows.max(1), columns.max(1)) {
            if let Err(error) = self
                .runtime
                .block_on(self.client.executions().resize(&self.execution, size))
            {
                hl_log::hl_error!(
                    hl_log::tag::RUNTIME,
                    "workspace terminal resize failed execution={} columns={} rows={} error={error}",
                    self.execution,
                    columns,
                    rows
                );
            }
        }
    }

    fn master_fd(&self) -> Option<RawFd> {
        None
    }

    fn try_wait(&mut self) -> Option<i32> {
        self.output
            .finished()
            .then(|| *self.exited.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            .flatten()
    }
}

impl Drop for ExecPty {
    fn drop(&mut self) {
        let live = self.try_wait().is_none();
        let client = self.client.clone();
        let execution = self.execution.clone();
        let failures = self.runtime.block_on(cleanup_with(
            live,
            CLEANUP_STEP_TIMEOUT,
            || async { client.executions().signal(&execution, "KILL").await },
            || async { client.executions().wait(&execution).await.map(drop) },
            || async { client.executions().remove(&execution).await },
        ));
        for failure in failures {
            hl_log::hl_error!(
                hl_log::tag::RUNTIME,
                "workspace execution cleanup failed execution={} {failure}",
                self.execution
            );
        }
        if let Some(pane) = &self.pane {
            if let Err(error) = pane.clear(&self.execution) {
                hl_log::hl_error!(
                    hl_log::tag::RUNTIME,
                    "workspace execution pane cleanup failed execution={} error={error}",
                    self.execution
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cleanup_with, Output};

    #[test]
    fn output_finishes_only_after_every_chunk_is_drained() {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(b"last ".to_vec()).unwrap();
        sender.send(b"line\n".to_vec()).unwrap();
        drop(sender);
        let mut output = Output::new(receiver);
        let mut bytes = [0; 5];

        let count = output.read(&mut bytes);

        assert_eq!(&bytes[..count], b"last ");
        assert!(!output.finished());
        let count = output.read(&mut bytes);
        assert_eq!(&bytes[..count], b"line\n");
        assert!(!output.finished());
        assert_eq!(output.read(&mut bytes), 0);
        assert!(output.finished());
    }

    #[test]
    fn cleanup_attempts_every_stage_after_each_failure() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let events = std::sync::Mutex::new(Vec::new());

        let failures = runtime.block_on(cleanup_with(
            true,
            std::time::Duration::from_secs(1),
            || async {
                events.lock().unwrap().push("signal");
                Err::<(), _>("signal failed")
            },
            || async {
                events.lock().unwrap().push("wait");
                Err::<(), _>("wait failed")
            },
            || async {
                events.lock().unwrap().push("remove");
                Err::<(), _>("remove failed")
            },
        ));

        assert_eq!(*events.lock().unwrap(), ["signal", "wait", "remove"]);
        assert_eq!(
            failures,
            ["signal: signal failed", "wait: wait failed", "remove: remove failed"]
        );
    }

    #[test]
    fn cleanup_is_bounded_and_still_attempts_remove_after_wait_timeout() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let removed = std::sync::atomic::AtomicBool::new(false);
        let started = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(10);

        let failures = runtime.block_on(cleanup_with(
            true,
            timeout,
            || async { Ok::<_, &'static str>(()) },
            std::future::pending::<Result<(), &'static str>>,
            || async {
                removed.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, &'static str>(())
            },
        ));

        assert!(removed.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(failures, ["wait: timed out"]);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn exited_execution_skips_process_control_but_is_still_removed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let events = std::sync::Mutex::new(Vec::new());

        let failures = runtime.block_on(cleanup_with(
            false,
            std::time::Duration::from_secs(1),
            || async {
                events.lock().unwrap().push("signal");
                Ok::<_, &'static str>(())
            },
            || async {
                events.lock().unwrap().push("wait");
                Ok::<_, &'static str>(())
            },
            || async {
                events.lock().unwrap().push("remove");
                Ok::<_, &'static str>(())
            },
        ));

        assert!(failures.is_empty());
        assert_eq!(*events.lock().unwrap(), ["remove"]);
    }
}
