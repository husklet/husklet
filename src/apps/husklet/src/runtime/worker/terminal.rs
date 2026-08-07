// The controlling-terminal and descriptor calls in this worker adapter are `unsafe` libc entry points.
#![allow(unsafe_code)]

use super::{on_winch, Ordering, WINCH};
use crate::ffi::RawMode;
use std::io::Write;

/// Establishes the terminal contract inherited by an engine-backed session.
///
/// The macOS GUI starts workers with `POSIX_SPAWN_SETSID`. That creates a new
/// session, but it does not reliably make the slave opened by the spawn file
/// actions the controlling terminal. Claim it after `exec`, when doing so is
/// safe, before the engine snapshots the worker's terminal descriptors.
pub(super) struct ControllingTerminal;

impl ControllingTerminal {
    pub(super) fn claim() -> std::io::Result<()> {
        let descriptor = libc::STDIN_FILENO;
        // SAFETY: `isatty` only inspects a descriptor number and cannot fault.
        if unsafe { libc::isatty(descriptor) } != 1 {
            return Ok(());
        }

        // SAFETY: both calls only name the inherited stdin descriptor.
        if unsafe { libc::tcgetpgrp(descriptor) } < 0
            && unsafe { libc::ioctl(descriptor, libc::TIOCSCTTY as libc::c_ulong, 0) } < 0
        {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: `getpgrp` takes no argument and `tcsetpgrp` names stdin and this process group.
        let group = unsafe { libc::getpgrp() };
        if group < 0 || unsafe { libc::tcsetpgrp(descriptor, group) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Host descriptor capacity required by engine-backed workspaces.
pub(super) struct OpenFiles;

impl OpenFiles {
    const REQUIRED: libc::rlim_t = 65_536;

    fn target(current: libc::rlim_t, maximum: libc::rlim_t) -> std::io::Result<Option<libc::rlim_t>> {
        if current >= Self::REQUIRED {
            return Ok(None);
        }
        let target = Self::REQUIRED.min(maximum);
        if target < Self::REQUIRED {
            return Err(std::io::Error::other(format!(
                "host permits only {target} open files; {} required",
                Self::REQUIRED
            )));
        }
        Ok(Some(target))
    }

    pub(super) fn prepare() -> std::io::Result<()> {
        let mut limit = Self::read_limit()?;
        let Some(target) = Self::target(limit.rlim_cur, limit.rlim_max)? else {
            return Ok(());
        };
        limit.rlim_cur = target;
        Self::write_limit(&limit)
    }
}

/// Private Unix resource-limit boundary implemented by its existing owner.
mod ffi {
    use super::{on_winch, OpenFiles, TerminalSession};

    impl TerminalSession<'_> {
        pub(super) fn install_resize_handler() {
            // SAFETY: `on_winch` has the C signal-handler ABI, retains no borrowed storage, and
            // accesses only a process-lifetime atomic. `signal` retains only that function pointer,
            // aliases no Rust storage, and neither side can unwind across the ABI.
            unsafe {
                libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
            }
        }

        pub(super) fn pace() {
            let duration = libc::timespec {
                tv_sec: 0,
                tv_nsec: 10_000_000,
            };
            // SAFETY: `duration` is an initialized integer aggregate borrowed for this call.
            // `nanosleep` retains no pointer, accesses no aliased mutable Rust storage, invokes no
            // callback, and cannot unwind across the ABI. Interruption remains intentionally ignored.
            unsafe { libc::nanosleep(&duration, std::ptr::null_mut()) };
        }
    }

    impl OpenFiles {
        pub(super) fn read_limit() -> std::io::Result<libc::rlimit> {
            // SAFETY: `rlimit` is a C integer aggregate with no invalid bit patterns, references, or
            // destructor obligations. The kernel receives exclusive writable access for this call,
            // retains no pointer, invokes no callback, and cannot unwind across the ABI.
            let (status, limit) = unsafe {
                let mut limit: libc::rlimit = std::mem::zeroed();
                let status = libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
                (status, limit)
            };
            if status == 0 {
                Ok(limit)
            } else {
                Err(std::io::Error::last_os_error())
            }
        }

        pub(super) fn write_limit(limit: &libc::rlimit) -> std::io::Result<()> {
            // SAFETY: `limit` is a valid, immutably borrowed integer aggregate for the duration of the
            // call. The kernel retains no pointer, accesses no aliased mutable Rust storage, invokes no
            // callback, and cannot unwind across the ABI.
            if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, limit) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
    }
}

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
        Self::install_resize_handler();
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
            if self.drain_output().is_err() {
                return 1;
            }
            if let Some(code) = self.pty.try_wait() {
                if self.drain_output().is_err() {
                    return 1;
                }
                return code;
            }
        }
    }

    fn resize_if_needed(&mut self) {
        let winched = WINCH.swap(false, Ordering::SeqCst);
        self.ticks = self.ticks.wrapping_add(1);
        if !winched && !self.ticks.is_multiple_of(30) {
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
        // SAFETY: `descriptor` is a live, fully initialised `pollfd` owned by this frame.
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
        // SAFETY: the destination is this frame's buffer and the length is its own capacity.
        let count = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                self.buffer.as_mut_ptr() as *mut _,
                self.buffer.len(),
            )
        };
        if count > 0 {
            self.pty.write(&self.buffer[..count as usize])?;
            return Ok(());
        }
        if count == 0 {
            self.pty.write(&[])?;
        }
        self.stdin_open = false;
        Ok(())
    }

    fn drain_output(&mut self) -> std::io::Result<()> {
        let mut wrote = false;
        loop {
            let count = self.pty.read(&mut self.buffer)?;
            if count == 0 {
                break;
            }
            self.output.write_all(&self.buffer[..count])?;
            wrote = true;
        }
        if wrote {
            self.output.flush()?;
        }
        Ok(())
    }
}

/// Query the controlling terminal's size (cols, rows).
pub(super) fn size() -> Option<(u16, u16)> {
    // SAFETY: `winsize` is plain data, and the pointer names this frame's live value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

pub(super) fn contract() -> String {
    let descriptor = libc::STDIN_FILENO;
    // SAFETY: `ttyname` returns null or a buffer valid until the next call on this thread.
    let terminal = unsafe {
        let name = libc::ttyname(descriptor);
        if name.is_null() {
            "?".to_owned()
        } else {
            std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned()
        }
    };
    // SAFETY: `rlimit` is plain data, and the pointer names this frame's live value.
    let mut nofile: libc::rlimit = unsafe { std::mem::zeroed() };
    let _ = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut nofile) };
    let mut environment: Vec<_> = std::env::vars().collect();
    environment.sort_by(|left, right| left.0.cmp(&right.0));
    format!(
        "terminal tty={terminal} sid={} pgid={} foreground={} size={:?} cwd={} nofile={}/{} descriptors=[{}] environment={environment:?}",
        unsafe { libc::getsid(0) },
        unsafe { libc::getpgrp() },
        unsafe { libc::tcgetpgrp(descriptor) },
        size(),
        std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "?".to_owned()),
        nofile.rlim_cur,
        nofile.rlim_max,
        Descriptors::open().join(", "),
    )
}

struct Descriptors;

impl Descriptors {
    fn open() -> Vec<String> {
        let Ok(entries) = std::fs::read_dir("/dev/fd") else {
            return Vec::new();
        };
        let mut descriptors: Vec<_> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let descriptor = name.parse::<libc::c_int>().ok()?;
                // SAFETY: `fcntl(F_GETFD)` only reads the flags of a descriptor number.
                let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
                let target = std::fs::read_link(entry.path())
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "?".to_owned());
                Some((descriptor, format!("{descriptor}:{flags:#x}:{target}")))
            })
            .collect();
        descriptors.sort_by_key(|(descriptor, _)| *descriptor);
        descriptors.into_iter().map(|(_, value)| value).collect()
    }
}

#[cfg(test)]
mod open_files_tests {
    use super::{OpenFiles, TerminalSession};
    use hl_ws_term::PtyBackend;
    use std::io;
    use std::os::fd::RawFd;

    #[test]
    fn raises_launchservices_limit_to_engine_capacity() {
        assert_eq!(OpenFiles::target(2_560, u64::MAX).unwrap(), Some(65_536));
        assert_eq!(OpenFiles::target(184_320, u64::MAX).unwrap(), None);
        assert!(OpenFiles::target(2_560, 32_768).is_err());
    }

    #[test]
    fn terminal_transport_errors_end_the_relay() {
        let mut backend = FailingBackend;
        let mut terminal = TerminalSession::new(&mut backend);

        let error = terminal.drain_output().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    }

    struct FailingBackend;

    impl PtyBackend for FailingBackend {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::ConnectionReset.into())
        }

        fn resize(&mut self, _columns: u16, _rows: u16) {}

        fn master_fd(&self) -> Option<RawFd> {
            None
        }

        fn try_wait(&mut self) -> Option<i32> {
            None
        }
    }
}
