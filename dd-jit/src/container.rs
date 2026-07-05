//! Async launch + supervision of a container process — the runtime capability a container manager
//! (e.g. `dd-daemon`) builds Docker semantics on top of. [`Runtime::start`] spawns the engine for a
//! [`Container`], pumps its stdio into a log broadcast (with rotation), and returns a
//! [`RunningContainer`] handle to `wait`/`signal`/`pause`/`resume`, feed stdin, and subscribe to output.
//!
//! Two stdio modes: a real PTY (`tty = true`) — an interactive shell sees a terminal, stdout+stderr
//! merge into one raw stream — or piped stdio (the multiplexed-frame path). The guest is placed in its
//! own process group so `pause`/`resume` (SIGSTOP/SIGCONT) reach the whole container via `killpg`.

use crate::api::{Container, Error, Runtime};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
    child: tokio::process::Child,
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
/// its own log broadcast / reaper) — see [`Runtime::start_into`]. The caller drives `child` (wait) and
/// drains `io_handles` on exit.
pub struct Launched {
    pub child: tokio::process::Child,
    pub pid: u32,
    /// PTY master fd (window-size ioctls) when `tty`, else `None`.
    pub pty_master: Option<RawFd>,
    pub io_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Runtime {
    /// Spawn the container's guest process and supervise its IO. Returns a [`RunningContainer`] handle.
    /// The engine is launched via the backend's command; the guest runs in its own process group.
    pub fn start(&self, c: &Container, io: Stdio3) -> Result<RunningContainer, Error> {
        let (out, _) = broadcast::channel::<(u8, Vec<u8>)>(1024);
        let log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (out_done, _) = watch::channel(false);
        let (exit, _) = watch::channel(None);
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(256);

        let launched = self.start_into(c, io, out.clone(), log_chunks.clone(), stdin_rx)?;
        Ok(RunningContainer {
            child: launched.child,
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
        let (prog, args) = c.command().ok_or(Error::NoBackend(c.guest()))?;
        let mut cmd = tokio::process::Command::new(prog);
        cmd.args(args);
        let (child, pty_master, io_handles) = if io.tty {
            spawn_tty(&mut cmd, out, log_chunks, stdin_rx)?
        } else {
            spawn_piped(&mut cmd, out, log_chunks, stdin_rx)?
        };
        let pid = child.id().unwrap_or(0);
        Ok(Launched { child, pid, pty_master, io_handles })
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
        let code = self.child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1) as i64;
        self.pty_master = None;
        let _ = self.exit.send(Some(code));
        for h in self.io_handles.drain(..) {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), h).await;
        }
        let _ = self.out_done.send(true);
        code
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

/// Pump a piped reader into the broadcast + rotated log under `kind` (1=stdout, 2=stderr).
async fn pump(
    mut r: impl AsyncReadExt + Unpin,
    kind: u8,
    out: broadcast::Sender<(u8, Vec<u8>)>,
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = buf[..n].to_vec();
                let _ = out.send((kind, chunk.clone()));
                push_log(&log_chunks, now_secs(), kind, chunk).await;
            }
        }
    }
}

type Spawned = (tokio::process::Child, Option<RawFd>, Vec<tokio::task::JoinHandle<()>>);

/// Piped stdio: own process group (setpgid) so pause/unpause SIGSTOP/SIGCONT the whole container.
fn spawn_piped(
    cmd: &mut tokio::process::Command,
    out: broadcast::Sender<(u8, Vec<u8>)>,
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
    mut stdin_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<Spawned, Error> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: setpgid(0,0) in the forked child is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(Error::Io)?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    if let Some(mut child_in) = child.stdin.take() {
        tokio::spawn(async move {
            while let Some(chunk) = stdin_rx.recv().await {
                if chunk.is_empty() {
                    break;
                }
                if child_in.write_all(&chunk).await.is_err() {
                    break;
                }
                let _ = child_in.flush().await;
            }
            drop(child_in); // EOF to the guest
        });
    }
    let h_out = tokio::spawn(pump(stdout, 1, out.clone(), log_chunks.clone()));
    let h_err = tokio::spawn(pump(stderr, 2, out, log_chunks));
    Ok((child, None, vec![h_out, h_err]))
}

/// PTY stdio: the guest gets a controlling terminal (login_tty in the forked child); the master is
/// pumped as one merged raw stream (kind 1) and fed from stdin.
fn spawn_tty(
    cmd: &mut tokio::process::Command,
    out: broadcast::Sender<(u8, Vec<u8>)>,
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
    mut stdin_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<Spawned, Error> {
    let (master, slave) = open_pty()?;
    let slave_fd = slave.as_raw_fd();
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: login_tty makes the slave the controlling terminal + stdin/out/err in the forked child.
    unsafe {
        cmd.pre_exec(move || {
            if libc::login_tty(slave_fd) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(Error::Io)?;
    drop(slave); // the child dup'd it via login_tty; close the parent's copy
    set_nonblocking(master.as_raw_fd());
    let master_fd = master.as_raw_fd();
    let afd = Arc::new(AsyncFd::new(master).map_err(Error::Io)?);
    // client stdin -> PTY master
    {
        let w = afd.clone();
        tokio::spawn(async move {
            while let Some(chunk) = stdin_rx.recv().await {
                if chunk.is_empty() {
                    break;
                }
                let mut off = 0;
                while off < chunk.len() {
                    let Ok(mut g) = w.writable().await else { return };
                    match g.try_io(|i| pty_write(i.as_raw_fd(), &chunk[off..])) {
                        Ok(Ok(n)) => off += n,
                        Ok(Err(_)) => return,
                        Err(_would_block) => continue,
                    }
                }
            }
        });
    }
    // PTY master -> broadcast (kind 1) + rotated log
    let reader = {
        let r = afd.clone();
        tokio::spawn(async move {
            loop {
                let Ok(mut g) = r.readable().await else { break };
                let mut buf = [0u8; 8192];
                match g.try_io(|i| pty_read(i.as_raw_fd(), &mut buf)) {
                    Ok(Ok(0)) | Ok(Err(_)) => break, // EOF / EIO when the guest exits
                    Ok(Ok(n)) => {
                        let chunk = buf[..n].to_vec();
                        let _ = out.send((1, chunk.clone()));
                        push_log(&log_chunks, now_secs(), 1, chunk).await;
                    }
                    Err(_would_block) => continue,
                }
            }
        })
    };
    Ok((child, Some(master_fd), vec![reader]))
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

fn set_nonblocking(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

fn pty_read(fd: RawFd, buf: &mut [u8]) -> std::io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn pty_write(fd: RawFd, buf: &[u8]) -> std::io::Result<usize> {
    let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}
