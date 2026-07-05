//! Async launch + supervision of a container process — the runtime capability a container manager
//! (e.g. `dd-daemon`) builds Docker semantics on top of. [`Runtime::start`] spawns the engine for a
//! [`Container`], pumps its stdio into a log broadcast (with rotation), and returns a
//! [`RunningContainer`] handle to `wait`/`signal`/`pause`/`resume`, feed stdin, and subscribe to output.
//!
//! Two stdio modes: a real PTY (`tty = true`) — an interactive shell sees a terminal, stdout+stderr
//! merge into one raw stream — or piped stdio (the multiplexed-frame path). The guest is placed in its
//! own process group so `pause`/`resume` (SIGSTOP/SIGCONT) reach the whole container via `killpg`.
//!
//! The engine is forked directly through the typed FFI (`dd_jit_darwin::spawn_io`): C owns the
//! `fork`/`execve`, the child's stdio (the pipes/pty this module opens) and its process-group/terminal
//! placement; this module owns the parent-side pipe/pty ends (async pumps) and reaps the pid.

use super::container::Container;
use super::error::Error;
use super::runtime::Runtime;
use dd_jit_darwin::{Guest, LaunchConfig, SpawnIo};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::sync::{broadcast, mpsc, watch};

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

/// A launched, supervised container process. Drop does not kill the guest; call [`signal`] to stop it.
pub struct RunningContainer {
    /// The guest's host pid (also its process-group id); reaped with `waitpid(2)`.
    pid: u32,
    /// Live output fan-out: every stdout/stderr chunk as `(stream, bytes)`. Subscribe with [`subscribe`].
    out: broadcast::Sender<(u8, Vec<u8>)>,
    /// The ordered, rotated replay buffer for `docker logs`-style replay.
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
    /// Set once, when output is fully drained after exit.
    out_done: watch::Sender<bool>,
    /// The exit code once the guest exits (`None` until then).
    exit: watch::Sender<Option<i64>>,
    /// Sink for stdin bytes (an empty Vec closes the guest's stdin).
    stdin_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// PTY master fd when `tty` — for window-size ioctls; `None` on the piped path.
    pty_master: Option<RawFd>,
    io_handles: Vec<tokio::task::JoinHandle<()>>,
}

/// The launched guest process + its IO reader tasks, when a caller supervises the process itself (feeds
/// its own log broadcast / reaper) — see [`Runtime::start_into`]. The caller reaps the pid (via [`wait`])
/// and drains `io_handles` on exit.
pub struct Launched {
    /// The guest's host pid (also its process-group id).
    pub pid: u32,
    /// PTY master fd (window-size ioctls) when `tty`, else `None`.
    pub pty_master: Option<RawFd>,
    pub io_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Launched {
    /// Reap the guest process, returning its exit code (-1 if killed by signal / unavailable). Does NOT
    /// drain `io_handles` — the caller owns them, so it can fire its own exit signal the instant the
    /// process dies, BEFORE waiting on readers that a stray fd-holding grandchild could keep open.
    pub async fn wait(&mut self) -> i64 {
        reap(self.pid).await
    }
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
    /// signalling and reaping via the returned [`Launched`]. Behavior matches [`start`]; only the channel
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
        if !dd_jit_darwin::available(c.guest()) {
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

    /// Block until the guest exits, returning its exit code (-1 if killed by signal / unavailable).
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

/// Reap `pid` on a blocking thread, decoding `waitpid` status into an exit code (-1 when signalled).
async fn reap(pid: u32) -> i64 {
    tokio::task::spawn_blocking(move || {
        let mut status: i32 = 0;
        // SAFETY: waitpid on our own forked child's pid with a valid status out-pointer.
        let r = unsafe { libc::waitpid(pid as i32, &mut status, 0) };
        if r < 0 {
            -1
        } else {
            decode_status(status)
        }
    })
    .await
    .unwrap_or(-1)
}

/// Decode a `waitpid` status: the normal exit code, or -1 when the child was terminated by a signal.
fn decode_status(status: i32) -> i64 {
    if status & 0x7f == 0 {
        ((status >> 8) & 0xff) as i64
    } else {
        -1
    }
}

/// Append one chunk to the rotated replay buffer, enforcing [`LOG_CHUNKS_CAP_BYTES`] by draining the
/// oldest chunks (but always keeping the just-pushed one).
async fn push_log(log_chunks: &Arc<tokio::sync::Mutex<Vec<LogChunk>>>, ts: i64, stream: u8, bytes: Vec<u8>) {
    let mut log = log_chunks.lock().await;
    log.push((ts, stream, bytes));
    let mut total: usize = log.iter().map(|(_, _, b)| b.len()).sum();
    let mut drop_to = 0;
    while total > LOG_CHUNKS_CAP_BYTES && drop_to < log.len() - 1 {
        total -= log[drop_to].2.len();
        drop_to += 1;
    }
    if drop_to > 0 {
        log.drain(..drop_to);
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The parent-side spawn result: `(pid, pty_master_fd, io_reader_tasks)`.
type Spawned = (u32, Option<RawFd>, Vec<tokio::task::JoinHandle<()>>);

/// Piped stdio: three pipes with the child on their fd 0/1/2, the child in its own process group
/// (setpgid) so pause/unpause SIGSTOP/SIGCONT the whole container. The parent keeps the write end of
/// stdin and the read ends of stdout/stderr as nonblocking [`AsyncFd`]s.
fn spawn_piped(
    guest: Guest,
    lc: &LaunchConfig,
    out: broadcast::Sender<(u8, Vec<u8>)>,
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
    stdin_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<Spawned, Error> {
    let (in_r, in_w) = pipe_cloexec()?;
    let (out_r, out_w) = pipe_cloexec()?;
    let (err_r, err_w) = pipe_cloexec()?;
    // C forks and dup2's the child-side ends onto fd 0/1/2, then execs the engine. Our copies of those
    // child-side fds are ours to close afterwards (the shim never closes caller fds).
    let pid = dd_jit_darwin::spawn_io(
        guest,
        lc,
        SpawnIo {
            stdin: in_r.as_raw_fd(),
            stdout: out_w.as_raw_fd(),
            stderr: err_w.as_raw_fd(),
            setpgid: true,
            tty: false,
        },
    )
    .map_err(Error::Io)?;
    drop(in_r);
    drop(out_w);
    drop(err_w);
    // Parent ends → nonblocking AsyncFd for the pump/stdin tasks.
    set_nonblocking(in_w.as_raw_fd());
    set_nonblocking(out_r.as_raw_fd());
    set_nonblocking(err_r.as_raw_fd());
    let in_w = Arc::new(AsyncFd::new(in_w).map_err(Error::Io)?);
    let out_r = Arc::new(AsyncFd::new(out_r).map_err(Error::Io)?);
    let err_r = Arc::new(AsyncFd::new(err_r).map_err(Error::Io)?);
    feed_stdin(in_w, stdin_rx);
    let h_out = tokio::spawn(pump_fd(out_r, 1, out.clone(), log_chunks.clone()));
    let h_err = tokio::spawn(pump_fd(err_r, 2, out, log_chunks));
    Ok((pid, None, vec![h_out, h_err]))
}

/// PTY stdio: the guest gets a controlling terminal (the C shim does setsid + TIOCSCTTY on the slave in
/// the forked child); the master is pumped as one merged raw stream (kind 1) and fed from stdin.
fn spawn_tty(
    guest: Guest,
    lc: &LaunchConfig,
    out: broadcast::Sender<(u8, Vec<u8>)>,
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
    stdin_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<Spawned, Error> {
    let (master, slave) = open_pty()?;
    // The master must not leak into the child; the child's slave copy is the dup2 source and is closed
    // on exec (both originals are close-on-exec; dup2 clears it on the fd 0/1/2 targets the child keeps).
    set_cloexec(master.as_raw_fd());
    set_cloexec(slave.as_raw_fd());
    let slave_fd = slave.as_raw_fd();
    let pid = dd_jit_darwin::spawn_io(
        guest,
        lc,
        SpawnIo { stdin: slave_fd, stdout: slave_fd, stderr: slave_fd, setpgid: false, tty: true },
    )
    .map_err(Error::Io)?;
    drop(slave); // the child dup'd it as its ctty; close the parent's copy
    set_nonblocking(master.as_raw_fd());
    let master_fd = master.as_raw_fd();
    let afd = Arc::new(AsyncFd::new(master).map_err(Error::Io)?);
    feed_stdin(afd.clone(), stdin_rx);
    let reader = tokio::spawn(pump_fd(afd, 1, out, log_chunks));
    Ok((pid, Some(master_fd), vec![reader]))
}

/// Feed the stdin channel into a writable fd (the stdin pipe write end, or the PTY master). An empty
/// chunk (or a closed channel) ends the task, dropping the fd → EOF to the guest.
fn feed_stdin(afd: Arc<AsyncFd<OwnedFd>>, mut stdin_rx: mpsc::Receiver<Vec<u8>>) {
    tokio::spawn(async move {
        while let Some(chunk) = stdin_rx.recv().await {
            if chunk.is_empty() {
                break;
            }
            let mut off = 0;
            while off < chunk.len() {
                let Ok(mut g) = afd.writable().await else { return };
                match g.try_io(|i| write_fd(i.as_raw_fd(), &chunk[off..])) {
                    Ok(Ok(n)) => off += n,
                    Ok(Err(_)) => return,
                    Err(_would_block) => continue,
                }
            }
        }
    });
}

/// Pump a readable fd (stdout/stderr pipe read end, or the PTY master) into the broadcast + rotated log
/// under `kind` (1=stdout, 2=stderr; a PTY merges to 1). Ends on EOF / EIO when the guest exits.
async fn pump_fd(
    afd: Arc<AsyncFd<OwnedFd>>,
    kind: u8,
    out: broadcast::Sender<(u8, Vec<u8>)>,
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
) {
    loop {
        let Ok(mut g) = afd.readable().await else { break };
        let mut buf = [0u8; 8192];
        match g.try_io(|i| read_fd(i.as_raw_fd(), &mut buf)) {
            Ok(Ok(0)) | Ok(Err(_)) => break, // EOF / EIO when the guest exits
            Ok(Ok(n)) => {
                let chunk = buf[..n].to_vec();
                let _ = out.send((kind, chunk.clone()));
                push_log(&log_chunks, now_secs(), kind, chunk).await;
            }
            Err(_would_block) => continue,
        }
    }
}

/// A `pipe(2)` whose two ends are both close-on-exec, wrapped as owned fds. Close-on-exec keeps the
/// parent-side ends from leaking into the forked child (which would stop the read ends from ever seeing
/// EOF); the child-side ends are re-exposed on fd 0/1/2 by the shim's dup2 (which clears close-on-exec).
fn pipe_cloexec() -> Result<(OwnedFd, OwnedFd), Error> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: libc::pipe fills a 2-element array with two fresh, valid fds on success.
    let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if r != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: pipe just handed us two fresh owned fds (read end fds[0], write end fds[1]).
    let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    set_cloexec(rd.as_raw_fd());
    set_cloexec(wr.as_raw_fd());
    Ok((rd, wr))
}

fn open_pty() -> Result<(OwnedFd, OwnedFd), Error> {
    let (mut m, mut s): (RawFd, RawFd) = (-1, -1);
    // Seed a sane 80x24 winsize at creation (matches docker/containerd's default ConsoleSize) instead of
    // the kernel's 0x0. A TUI (htop, vim, less) reads TIOCGWINSZ at startup -- possibly BEFORE the client's
    // first /resize lands, or when the client is `-t` without a real terminal -- and a 0x0 size makes
    // ncurses compute an empty screen -> a BLANK render. The real size still arrives via a later resize.
    let ws = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    // termios is *mut on macOS, *const on linux; null_mut() coerces to both.
    let r = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &ws as *const _ as *mut _,
        )
    };
    if r != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    // Safety: openpty just handed us two fresh owned fds.
    Ok(unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) })
}

fn set_cloexec(fd: RawFd) {
    // Safety: fcntl(F_SETFD) on an fd we own; a bad fd just returns -1, which we ignore.
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, fl | libc::FD_CLOEXEC);
    }
}

fn set_nonblocking(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

fn read_fd(fd: RawFd, buf: &mut [u8]) -> std::io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write_fd(fd: RawFd, buf: &[u8]) -> std::io::Result<usize> {
    let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}
