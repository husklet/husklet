pub(super) struct Shell;

impl Shell {
    pub(super) fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Sanitize a workspace name into a hostname/netns-safe token.
pub(super) struct Hostname;

impl Hostname {
    pub(super) fn sanitize(name: &str) -> String {
        let s: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let t = s.trim_matches('-');
        if t.is_empty() {
            "workspace".to_string()
        } else {
            t.to_string()
        }
    }
}

pub(super) struct WindowSize;

impl WindowSize {
    pub(super) fn parse(value: &str) -> Option<(u32, u32)> {
        let (width, height) = value
            .trim()
            .split_once(|character| character == ',' || character == 'x')?;
        let width: u32 = width.trim().parse().ok()?;
        let height: u32 = height.trim().parse().ok()?;
        (width > 0 && height > 0 && width <= 16384 && height <= 16384).then_some((width, height))
    }
}

/// A synchronous [`PtyBackend`] over a hl-jit-launched container: output drained from the pre-subscribed
/// broadcast, input pushed to the guest stdin channel, resize/reap via the master fd + pid.
pub(super) struct HlJitPty {
    /// Kept alive so hl-jit's IO pump tasks keep running (they feed the broadcast we drain).
    pub(super) _rt: tokio::runtime::Runtime,
    /// Owns the per-launch GPU endpoint for as long as the guest can use its injected socket.
    pub(super) _gpu_service: Option<crate::runtime::gpu::Service>,
    /// Owns the per-launch Wayland endpoint for as long as the guest can use it.
    pub(super) _compositor_service: Option<crate::runtime::compositor::Service>,
    pub(super) stdin_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub(super) rx: tokio::sync::broadcast::Receiver<(u8, Vec<u8>)>,
    pub(super) master: Option<RawFd>,
    pub(super) pid: libc::pid_t,
    /// Bytes received from the broadcast that didn't fit the last `read` buffer.
    pub(super) pending: VecDeque<u8>,
    pub(super) exited: Option<i32>,
}

impl PtyBackend for HlJitPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let _ = self.stdin_tx.try_send(bytes.to_vec());
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use tokio::sync::broadcast::error::TryRecvError;
        let mut n = 0;
        while n < buf.len() {
            if let Some(b) = self.pending.pop_front() {
                buf[n] = b;
                n += 1;
                continue;
            }
            match self.rx.try_recv() {
                Ok((_stream, bytes)) => self.pending.extend(bytes),
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue, // dropped under burst; keep draining
            }
        }
        Ok(n)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if let Some(fd) = self.master {
            let ws = libc::winsize {
                ws_row: rows.max(1),
                ws_col: cols.max(1),
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            unsafe {
                libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
            }
        }
    }

    fn master_fd(&self) -> Option<RawFd> {
        None // output is drained from the broadcast, not the fd (hl-jit's pump owns the fd)
    }

    fn try_wait(&mut self) -> Option<i32> {
        if self.exited.is_some() {
            return self.exited;
        }
        let mut status: libc::c_int = 0;
        let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if r == self.pid {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                -1
            };
            self.exited = Some(code);
        }
        self.exited
    }
}

impl Drop for HlJitPty {
    fn drop(&mut self) {
        // Stop the guest's process group (pid == pgid); the pumps end when the PTY closes. ESRCH (already
        // gone) is fine.
        if self.exited.is_none() {
            unsafe {
                libc::killpg(self.pid, libc::SIGHUP);
            }
        }
    }
}
use super::*;
