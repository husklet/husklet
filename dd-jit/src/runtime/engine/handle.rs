use super::*;
use std::os::fd::RawFd;

/// A launched, supervised container process. Drop does not kill the guest; call its `signal` method to stop it.
pub struct RunningContainer {
    /// The guest's host pid (also its process-group id); reaped with `waitpid(2)`.
    pub(super) pid: u32,
    /// Live output fan-out: every stdout/stderr chunk as `(stream, bytes)`. Subscribe with [`subscribe`].
    pub(super) out: broadcast::Sender<(u8, Vec<u8>)>,
    /// The ordered, rotated replay buffer for `docker logs`-style replay.
    pub(super) log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
    /// Set once, when output is fully drained after exit.
    pub(super) out_done: watch::Sender<bool>,
    /// The exit code once the guest exits (`None` until then).
    pub(super) exit: watch::Sender<Option<i64>>,
    /// Sink for stdin bytes (an empty Vec closes the guest's stdin).
    pub(super) stdin_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// PTY master fd when `tty` — for window-size ioctls; `None` on the piped path.
    pub(super) pty_master: Option<RawFd>,
    pub(super) io_handles: Vec<tokio::task::JoinHandle<()>>,
}

/// The launched guest process + its IO reader tasks, when a caller supervises the process itself (feeds
/// its own log broadcast / reaper) — see [`Runtime::start_into`]. The caller reaps the pid (via `wait`)
/// and drains `io_handles` on exit.
pub struct Launched {
    /// The guest's host pid (also its process-group id).
    pub pid: u32,
    /// PTY master fd (window-size ioctls) when `tty`, else `None`.
    pub pty_master: Option<RawFd>,
    /// The stdout/stderr reader tasks; the caller drains these on exit (after [`Launched::wait`] reaps the pid).
    pub io_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Launched {
    /// Reap the guest process, returning its exit code (`128+signum` if killed by signal). Does NOT
    /// drain `io_handles` — the caller owns them, so it can fire its own exit signal the instant the
    /// process dies, BEFORE waiting on readers that a stray fd-holding grandchild could keep open.
    pub async fn wait(&mut self) -> i64 {
        reap(self.pid).await
    }
}

impl RunningContainer {
    /// The guest's host process id (also its process-group id).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The PTY master fd (window-size ioctls), or `None` on the piped path.
    pub fn pty_master(&self) -> Option<RawFd> {
        self.pty_master
    }

    /// Subscribe to the live output stream (`(stream, bytes)`; stream 1=stdout, 2=stderr).
    pub fn subscribe(&self) -> broadcast::Receiver<(u8, Vec<u8>)> {
        self.out.subscribe()
    }

    /// A watch that flips to `Some(code)` when the guest exits.
    pub fn exit_watch(&self) -> watch::Receiver<Option<i64>> {
        self.exit.subscribe()
    }

    /// A watch that flips to `true` once all output has been drained after exit (streamers wait on this
    /// so a fast-exiting command's tail is never lost).
    pub fn output_done(&self) -> watch::Receiver<bool> {
        self.out_done.subscribe()
    }

    /// The current ordered replay buffer (rotated to ≤ 8 MiB), oldest-first.
    pub async fn logs_snapshot(&self) -> Vec<LogChunk> {
        self.log_chunks.lock().await.clone()
    }

    /// Feed bytes to the guest's stdin. An empty slice closes stdin (EOF).
    pub fn write_stdin(&self, bytes: Vec<u8>) {
        if let Some(tx) = &self.stdin_tx {
            let _ = tx.try_send(bytes);
        }
    }

    /// Send a signal to the guest's process group (reaches the JIT plus any host processes it forked).
    pub fn signal(&self, sig: i32) -> Result<(), Error> {
        // Safety: killpg on our own child's pgid; a stale pgid just returns ESRCH (already gone → ok).
        let r = unsafe { libc::killpg(self.pid as i32, sig) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ESRCH) {
                return Err(Error::Io(e));
            }
        }
        Ok(())
    }

    /// Pause the whole container (SIGSTOP to the process group).
    pub fn pause(&self) -> Result<(), Error> {
        self.signal(libc::SIGSTOP)
    }

    /// Resume the whole container (SIGCONT to the process group).
    pub fn resume(&self) -> Result<(), Error> {
        self.signal(libc::SIGCONT)
    }

    /// Block until the guest exits, returning its exit code (`128+signum` if killed by signal).
    /// Fires the exit watch, then drains the IO readers (bounded grace) and fires output-done.
    pub async fn wait(&mut self) -> i64 {
        let code = reap(self.pid).await;
        self.pty_master = None;
        let _ = self.exit.send(Some(code));
        for h in self.io_handles.drain(..) {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), h).await;
        }
        let _ = self.out_done.send(true);
        code
    }
}
