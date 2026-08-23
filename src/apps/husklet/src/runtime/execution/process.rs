use hl_client::Client;
use hl_client::api::Size;
use hl_ws_term::PtyBackend;
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::os::unix::io::RawFd;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::PaneExecution;

pub(super) struct Shell;

impl Shell {
    pub(super) fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(super) struct ExecPty {
    pub(super) runtime: PaneRuntime,
    pub(super) client: Client,
    pub(super) execution: String,
    pub(super) input: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub(super) output: Output,
    pub(super) exited: Arc<Mutex<Option<i32>>>,
    pub(super) pane: Option<PaneExecution>,
}

pub(super) struct PaneRuntime {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl PaneRuntime {
    pub(super) fn shared() -> io::Result<Self> {
        static SHARED: OnceLock<Mutex<Weak<tokio::runtime::Runtime>>> = OnceLock::new();
        let mut shared = SHARED
            .get_or_init(|| Mutex::new(Weak::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let runtime = if let Some(runtime) = shared.upgrade() {
            runtime
        } else {
            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("hl-exec")
                    .enable_all()
                    .build()?,
            );
            *shared = Arc::downgrade(&runtime);
            runtime
        };
        Ok(Self {
            tasks: Vec::new(),
            runtime,
        })
    }

    pub(super) fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    pub(super) fn spawn(&mut self, future: impl Future<Output = ()> + Send + 'static) {
        self.tasks.push(self.runtime.spawn(future));
    }
}

impl Drop for PaneRuntime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
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
    receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
    closed: bool,
}

pub(super) const OUTPUT_QUEUE_RECORDS: usize = 64;

impl Output {
    pub(super) fn new(receiver: tokio::sync::mpsc::Receiver<Vec<u8>>) -> Self {
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
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
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
        if self.input.capacity() == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.input.try_send(bytes.to_vec()).map_err(|error| match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => io::ErrorKind::WouldBlock.into(),
            tokio::sync::mpsc::error::TrySendError::Closed(_) => io::ErrorKind::BrokenPipe.into(),
        })
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

    fn master_descriptor(&self) -> Option<RawFd> {
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
    use super::{Output, PaneRuntime, cleanup_with};

    #[test]
    fn output_finishes_only_after_every_chunk_is_drained() {
        let (sender, receiver) = tokio::sync::mpsc::channel(super::OUTPUT_QUEUE_RECORDS);
        sender.try_send(b"last ".to_vec()).unwrap();
        sender.try_send(b"line\n".to_vec()).unwrap();
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
    fn terminal_output_is_bounded_and_preserves_record_order() {
        let (sender, receiver) = tokio::sync::mpsc::channel(super::OUTPUT_QUEUE_RECORDS);
        for byte in 0..super::OUTPUT_QUEUE_RECORDS {
            sender.try_send(vec![byte as u8]).unwrap();
        }
        assert!(matches!(
            sender.try_send(vec![255]),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));

        let mut output = super::Output::new(receiver);
        let mut first = [0; 2];
        assert_eq!(output.read(&mut first), first.len());
        assert_eq!(first, [0, 1]);
        sender.try_send(vec![super::OUTPUT_QUEUE_RECORDS as u8]).unwrap();

        let mut remaining = [0; 128];
        let count = output.read(&mut remaining);
        assert_eq!(
            &remaining[..count],
            &(2..=super::OUTPUT_QUEUE_RECORDS as u8).collect::<Vec<_>>()
        );
        drop(sender);
        assert_eq!(output.read(&mut remaining), 0);
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

    #[cfg(target_os = "linux")]
    fn named_threads(name: &str) -> usize {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|comm| comm.trim() == name))
            .count()
    }

    #[cfg(target_os = "linux")]
    fn descriptors() -> usize {
        std::fs::read_dir("/proc/self/fd").unwrap().count()
    }

    #[test]
    fn dropping_one_pane_cancels_its_tasks_without_harming_its_sibling() {
        struct MarksDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

        impl Drop for MarksDrop {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let mut pane_a = PaneRuntime::shared().unwrap();
        let mut pane_b = PaneRuntime::shared().unwrap();
        assert!(std::sync::Arc::ptr_eq(&pane_a.runtime, &pane_b.runtime));

        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pending_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        pane_a.spawn({
            let cancelled = std::sync::Arc::clone(&cancelled);
            let pending_started = std::sync::Arc::clone(&pending_started);
            async move {
                let _cancelled = MarksDrop(cancelled);
                pending_started.store(true, std::sync::atomic::Ordering::Release);
                std::future::pending::<()>().await;
            }
        });
        let panic_unwound = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        pane_a.spawn({
            let panic_unwound = std::sync::Arc::clone(&panic_unwound);
            async move {
                let _panic_unwound = MarksDrop(panic_unwound);
                tokio::task::yield_now().await;
                panic!("intentional pane task panic");
            }
        });

        let heartbeat = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        pane_b.spawn({
            let heartbeat = std::sync::Arc::clone(&heartbeat);
            async move {
                loop {
                    heartbeat.fetch_add(1, std::sync::atomic::Ordering::Release);
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while (!pending_started.load(std::sync::atomic::Ordering::Acquire)
            || !panic_unwound.load(std::sync::atomic::Ordering::Acquire)
            || heartbeat.load(std::sync::atomic::Ordering::Acquire) < 2)
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert!(pending_started.load(std::sync::atomic::Ordering::Acquire));
        assert!(panic_unwound.load(std::sync::atomic::Ordering::Acquire));
        let before_drop = heartbeat.load(std::sync::atomic::Ordering::Acquire);

        drop(pane_a);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while (!cancelled.load(std::sync::atomic::Ordering::Acquire)
            || heartbeat.load(std::sync::atomic::Ordering::Acquire) <= before_drop)
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert!(
            cancelled.load(std::sync::atomic::Ordering::Acquire),
            "pane A task survived its owner"
        );
        assert!(
            heartbeat.load(std::sync::atomic::Ordering::Acquire) > before_drop,
            "pane A drop or panic stopped pane B"
        );
        drop(pane_b);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn eight_fake_panes_share_workers_and_drop_tasks_and_runtime_to_baseline() {
        const CHILD: &str = "HL_EXEC_RUNTIME_CENSUS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("eight_fake_panes_share_workers_and_drop_tasks_and_runtime_to_baseline")
                .arg("--nocapture")
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated pane census failed");
            return;
        }

        struct Active(std::sync::Arc<std::sync::atomic::AtomicUsize>);

        impl Drop for Active {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::Release);
            }
        }

        // Tokio installs two process-lifetime driver descriptors on first use. Warm that singleton before
        // taking the zero-pane baseline; pane-owned descriptors must still return exactly to that baseline.
        drop(PaneRuntime::shared().unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while named_threads("hl-exec") != 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let baseline_fds = descriptors();
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut panes = Vec::new();
        for _ in 0..8 {
            let mut pane = PaneRuntime::shared().unwrap();
            for _ in 0..3 {
                let active = std::sync::Arc::clone(&active);
                pane.spawn(async move {
                    active.fetch_add(1, std::sync::atomic::Ordering::Release);
                    let _active = Active(active);
                    std::future::pending::<()>().await;
                });
            }
            panes.push(pane);
        }
        assert!(
            panes[1..]
                .iter()
                .all(|pane| std::sync::Arc::ptr_eq(&panes[0].runtime, &pane.runtime))
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while (active.load(std::sync::atomic::Ordering::Acquire) != 24 || named_threads("hl-exec") != 2)
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), 24);
        assert_eq!(
            named_threads("hl-exec"),
            2,
            "eight panes created more than one worker pair"
        );
        let held_fds = descriptors();

        drop(panes);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while (active.load(std::sync::atomic::Ordering::Acquire) != 0 || named_threads("hl-exec") != 0)
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let final_fds = descriptors();
        println!("execution-runtime panes=8 fds={baseline_fds}/{held_fds}/{final_fds}");
        assert_eq!(
            active.load(std::sync::atomic::Ordering::Acquire),
            0,
            "pane tasks survived their owner"
        );
        assert_eq!(
            named_threads("hl-exec"),
            0,
            "the last pane left immortal runtime workers"
        );
        assert!(
            held_fds > baseline_fds,
            "runtime did not open any observable descriptors"
        );
        assert_eq!(final_fds, baseline_fds, "pane drop did not restore descriptor baseline");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual execution-runtime census used by /var/tmp/execution-runtime-abba.sh"]
    fn execution_runtime_census() {
        let panes = std::env::var("HL_EXEC_RUNTIME_PANES")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mode = std::env::var("HL_EXEC_RUNTIME_MODE").unwrap();
        let baseline_fds = descriptors();
        let started = std::time::Instant::now();
        let mut shared = Vec::new();
        let mut isolated = Vec::new();
        match mode.as_str() {
            "shared" => {
                for _ in 0..panes {
                    shared.push(PaneRuntime::shared().unwrap());
                }
            }
            "isolated" => {
                for _ in 0..panes {
                    isolated.push(
                        tokio::runtime::Builder::new_multi_thread()
                            .worker_threads(2)
                            .thread_name("hl-exec-control")
                            .enable_all()
                            .build()
                            .unwrap(),
                    );
                }
            }
            _ => panic!("HL_EXEC_RUNTIME_MODE must be shared or isolated"),
        }
        let elapsed = started.elapsed();
        let worker_name = if mode == "shared" { "hl-exec" } else { "hl-exec-control" };
        let expected_workers = if mode == "shared" { 2 } else { panes * 2 };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while named_threads(worker_name) != expected_workers && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        println!(
            "execution-runtime mode={mode} panes={panes} elapsed_ns={} workers={} fds_delta={}",
            elapsed.as_nanos(),
            named_threads(worker_name),
            descriptors().saturating_sub(baseline_fds),
        );
        assert_eq!(named_threads(worker_name), expected_workers);
    }
}
