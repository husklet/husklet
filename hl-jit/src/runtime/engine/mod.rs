//! Async launch + supervision of a container process — the runtime capability a container manager
//! (e.g. `hl-daemon`) builds Docker semantics on top of. [`Runtime::start`] spawns the engine for a
//! [`Container`], pumps its stdio into a log broadcast (with rotation), and returns a
//! [`RunningContainer`] handle to `wait`/`signal`/`pause`/`resume`, feed stdin, and subscribe to output.
//!
//! Two stdio modes: a real PTY (`tty = true`) — an interactive shell sees a terminal, stdout+stderr
//! merge into one raw stream — or piped stdio (the multiplexed-frame path). The guest is placed in its
//! own process group so `pause`/`resume` (SIGSTOP/SIGCONT) reach the whole container via `killpg`.
//!
//! The engine is forked directly through the typed FFI (`hl_jit_darwin::spawn_io`): C owns the
//! `fork`/`execve`, the child's stdio (the pipes/pty this module opens) and its process-group/terminal
//! placement; this module owns the parent-side pipe/pty ends (async pumps) and reaps the pid.

use super::container::Container;
use super::error::Error;
use super::runtime::Runtime;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch};

mod handle;
mod io;
mod pump;

pub use handle::{Launched, RunningContainer};
pub(crate) use io::{read_fd, spawn_piped, spawn_tty};
pub(crate) use pump::{pump_fd, reap};

/// Cap on the retained log-replay buffer. A chatty/long-lived guest would otherwise grow the buffer
/// without bound; when a chunk pushes it over, the oldest chunks are dropped (log rotation), so the
/// replay shows the most-recent ≤ 8 MiB.
const LOG_CHUNKS_CAP_BYTES: usize = 8 * 1024 * 1024;

/// One output chunk: `(unix_secs, stream, bytes)` where stream is 1=stdout, 2=stderr (a PTY merges to 1).
pub type LogChunk = (i64, u8, Vec<u8>);

/// How to wire the guest's stdio.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stdio3 {
    /// Give the guest a real controlling PTY (docker `-t`): interactive terminal, merged raw stream.
    pub tty: bool,
}

impl Runtime {
    /// Spawn the container's guest process and supervise its IO. Returns a [`RunningContainer`] handle.
    /// The engine is forked via the typed FFI; the guest runs in its own process group.
    pub fn start(&self, c: &Container, io: Stdio3) -> Result<RunningContainer, Error> {
        let (out, _) = broadcast::channel::<(u8, Vec<u8>)>(1024);
        let log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (out_done, _) = watch::channel(false);
        let (exit, _) = watch::channel(None);
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(256);

        let launched = self.start_into(c, io, out.clone(), log_chunks.clone(), stdin_rx)?;
        Ok(RunningContainer {
            pid: launched.pid,
            out,
            log_chunks,
            out_done,
            exit,
            stdin_tx: Some(stdin_tx),
            pty_master: launched.pty_master,
            io_handles: launched.io_handles,
        })
    }

    /// Spawn + supervise the guest's IO INTO caller-provided channels — the container manager's path:
    /// the pump writes each output chunk into `out` (live fan-out) and appends it to `log_chunks` (the
    /// rotated replay buffer), and guest stdin is fed from `stdin_rx`. The caller owns exit/out-done
    /// signalling and reaping via the returned [`Launched`]. Behavior matches `start`; only the channel
    /// ownership differs (so e.g. a Docker daemon can bind attach/logs to these channels before start).
    pub fn start_into(
        &self,
        c: &Container,
        io: Stdio3,
        out: broadcast::Sender<(u8, Vec<u8>)>,
        log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
        stdin_rx: mpsc::Receiver<Vec<u8>>,
    ) -> Result<Launched, Error> {
        let c = self.with_defaults(c);
        if !hl_jit_darwin::available(c.guest()) {
            return Err(Error::NoBackend(c.guest()));
        }
        let guest = c.guest();
        let lc = c.launch_config();
        let (pid, pty_master, io_handles) = if io.tty {
            spawn_tty(guest, &lc, out, log_chunks, stdin_rx)?
        } else {
            spawn_piped(guest, &lc, out, log_chunks, stdin_rx)?
        };
        Ok(Launched { pid, pty_master, io_handles })
    }

    /// Run the container to completion and return `(exit_code, combined_output)` — the one-shot capture a
    /// build `RUN` step or a HEALTHCHECK probe needs (no live fan-out, no daemon bookkeeping). Reuses the
    /// piped-spawn machinery: stdout+stderr are pumped into a private ordered buffer (so the combined
    /// bytes come back in chronological chunk order), the pid is reaped for its exit code, and stdin is
    /// closed immediately (EOF). With `timeout = Some(dur)`, exceeding it SIGKILLs the guest's process
    /// group and returns `(-1, partial_output)` — the timed-out-probe contract the caller treats as
    /// unhealthy. A missing engine for the guest is `Err(Error::NoBackend)`.
    pub async fn output(
        &self,
        c: &Container,
        timeout: Option<std::time::Duration>,
    ) -> Result<(i64, Vec<u8>), Error> {
        let c = self.with_defaults(c);
        if !hl_jit_darwin::available(c.guest()) {
            return Err(Error::NoBackend(c.guest()));
        }
        let guest = c.guest();
        let lc = c.launch_config();
        // A private capture sink: the pumps append every stdout/stderr chunk here (in order). No live
        // broadcast receiver and no stdin — drop the stdin sender so the guest sees EOF at once.
        let (out, _) = broadcast::channel::<(u8, Vec<u8>)>(1024);
        let log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (_, stdin_rx) = mpsc::channel::<Vec<u8>>(1);
        let (pid, _pty, io_handles) = spawn_piped(guest, &lc, out, log_chunks.clone(), stdin_rx)?;

        // Reap the guest for its exit code, bounding the wait when a timeout is set. On elapse: SIGKILL
        // the process group, reap the corpse (no zombie), and report the timed-out sentinel (-1).
        let reaped = reap(pid);
        tokio::pin!(reaped);
        let code = match timeout {
            Some(dur) => match tokio::time::timeout(dur, &mut reaped).await {
                Ok(code) => code,
                Err(_) => {
                    // Safety: killpg on our own child's pgid; a stale pgid returns ESRCH (already gone).
                    unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
                    let _ = (&mut reaped).await; // reap the corpse; its signalled code is discarded for -1
                    -1
                }
            },
            None => reaped.await,
        };
        // Drain the pumps (bounded) so the last chunks land in the buffer, then concatenate in order.
        for h in io_handles {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), h).await;
        }
        let mut bytes = Vec::new();
        for (_, _, b) in log_chunks.lock().await.iter() {
            bytes.extend_from_slice(b);
        }
        Ok((code, bytes))
    }
}
