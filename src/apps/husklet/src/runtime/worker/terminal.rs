use super::{on_winch, Ordering, WINCH};
use std::io::Write;
use std::os::unix::io::RawFd;

/// Raw-mode passthrough between the real terminal and the workspace PTY until the child exits.
/// Backend-agnostic across local and engine-backed PTYs.
pub(super) struct TerminalSession<'a> {
    pty: &'a mut dyn hl_ws_term::PtyBackend,
    _raw: RawMode,
    buffer: [u8; 8192],
    output: std::io::Stdout,
    last_size: Option<(u16, u16)>,
    ticks: u32,
    stdin_open: bool,
}

impl<'a> TerminalSession<'a> {
    pub(super) fn run(pty: &'a mut dyn hl_ws_term::PtyBackend) -> i32 {
        Self::new(pty).drive()
    }

    fn new(pty: &'a mut dyn hl_ws_term::PtyBackend) -> Self {
        let raw = RawMode::enter(libc::STDIN_FILENO);
        unsafe {
            libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
        }
        let last_size = size();
        if let Some((columns, rows)) = last_size {
            pty.resize(columns, rows);
        }
        Self {
            pty,
            _raw: raw,
            buffer: [0; 8192],
            output: std::io::stdout(),
            last_size,
            ticks: 0,
            stdin_open: true,
        }
    }

    fn drive(mut self) -> i32 {
        loop {
            self.resize_if_needed();
            if self.poll_input().is_err() {
                return 1;
            }
            self.drain_output();
            if let Some(code) = self.pty.try_wait() {
                self.drain_output();
                return code;
            }
        }
    }

    fn resize_if_needed(&mut self) {
        let winched = WINCH.swap(false, Ordering::SeqCst);
        self.ticks = self.ticks.wrapping_add(1);
        if !winched && self.ticks % 30 != 0 {
            return;
        }
        let current = size();
        if current == self.last_size {
            return;
        }
        if let Some((columns, rows)) = current {
            self.pty.resize(columns, rows);
        }
        self.last_size = current;
    }

    fn poll_input(&mut self) -> std::io::Result<()> {
        if !self.stdin_open {
            Self::pace();
            return Ok(());
        }
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, 10) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
            return Ok(());
        }
        if result == 0 || descriptor.revents & libc::POLLIN == 0 {
            return Ok(());
        }
        let count = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                self.buffer.as_mut_ptr() as *mut _,
                self.buffer.len(),
            )
        };
        if count > 0 {
            let _ = self.pty.write(&self.buffer[..count as usize]);
            return Ok(());
        }
        if count == 0 {
            let _ = self.pty.write(&[]);
        }
        self.stdin_open = false;
        Ok(())
    }

    fn drain_output(&mut self) {
        let mut wrote = false;
        loop {
            let count = self.pty.read(&mut self.buffer).unwrap_or(0);
            if count == 0 {
                break;
            }
            let _ = self.output.write_all(&self.buffer[..count]);
            wrote = true;
        }
        if wrote {
            let _ = self.output.flush();
        }
    }

    fn pace() {
        let duration = libc::timespec {
            tv_sec: 0,
            tv_nsec: 10_000_000,
        };
        unsafe { libc::nanosleep(&duration, std::ptr::null_mut()) };
    }
}

/// Query the controlling terminal's size (cols, rows).
pub(super) fn size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

/// RAII raw-mode for a tty fd; restores the saved termios on drop.
struct RawMode {
    fd: RawFd,
    saved: Option<libc::termios>,
}
impl RawMode {
    fn enter(fd: RawFd) -> RawMode {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return RawMode { fd, saved: None }; // not a tty (e.g. piped) — leave as-is
            }
            let saved = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(fd, libc::TCSANOW, &t);
            RawMode {
                fd,
                saved: Some(saved),
            }
        }
    }
}
impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &saved);
            }
        }
    }
}
