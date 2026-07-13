use super::*;
use hl_jit_darwin::{Guest, LaunchConfig, SpawnIo};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use tokio::io::unix::AsyncFd;

/// The parent-side spawn result: `(pid, pty_master_fd, io_reader_tasks)`.
pub(crate) type Spawned = (u32, Option<RawFd>, Vec<tokio::task::JoinHandle<()>>);

/// Piped stdio: three pipes with the child on their fd 0/1/2, the child in its own process group
/// (setpgid) so pause/unpause SIGSTOP/SIGCONT the whole container. The parent keeps the write end of
/// stdin and the read ends of stdout/stderr as nonblocking [`AsyncFd`]s.
pub(crate) fn spawn_piped(
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
    let pid = hl_jit_darwin::spawn_io(
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
pub(crate) fn spawn_tty(
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
    let pid = hl_jit_darwin::spawn_io(
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

pub(crate) fn read_fd(fd: RawFd, buf: &mut [u8]) -> std::io::Result<usize> {
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
