// The controlling-terminal and descriptor calls in this worker adapter are `unsafe` libc entry points.
#![allow(unsafe_code)]

use super::{on_winch, Ordering, WINCH};
use crate::ffi::{InterruptMask, RawMode};
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
            unsafe { libc::nanosleep(&raw const duration, std::ptr::null_mut()) };
        }
    }

    impl OpenFiles {
        pub(super) fn read_limit() -> std::io::Result<libc::rlimit> {
            // SAFETY: `rlimit` is a C integer aggregate with no invalid bit patterns, references, or
            // destructor obligations. The kernel receives exclusive writable access for this call,
            // retains no pointer, invokes no callback, and cannot unwind across the ABI.
            let (status, limit) = unsafe {
                let mut limit: libc::rlimit = std::mem::zeroed();
                let status = libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit);
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
    pending_input: Option<Vec<u8>>,
}

// Keep a productive child from monopolising the relay loop. Each pass must return to
// input, resize, and exit handling even when more output is immediately available.
const OUTPUT_DRAIN_BUDGET: usize = 256 * 1024;

impl<'a> TerminalSession<'a> {
    pub(super) fn run(pty: &'a mut dyn hl_ws_term::PtyBackend, interrupts: InterruptMask) -> i32 {
        Self::new(pty, Some(interrupts)).drive()
    }

    fn new(pty: &'a mut dyn hl_ws_term::PtyBackend, interrupts: Option<InterruptMask>) -> Self {
        let raw = RawMode::enter(libc::STDIN_FILENO);
        // Raw mode is in effect: the line discipline no longer raises a signal, and Ctrl-C reaches
        // the guest as the 0x03 byte the relay forwards. Restore the inherited mask here so the
        // launch window is the only interval in which interrupts are held.
        if let Some(interrupts) = interrupts {
            interrupts.release();
        }
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
            pending_input: None,
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
            let Some(code) = self.pty.try_wait() else {
                if self.pending_input.is_some() {
                    Self::pace();
                }
                continue;
            };
            if self.drain_output().is_err() {
                return 1;
            }
            return code;
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
        if flush_pending_input(self.pty, &mut self.pending_input)? {
            return Ok(());
        }
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
        let result = unsafe { libc::poll(&raw mut descriptor, 1, 10) };
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
        let count = unsafe { libc::read(libc::STDIN_FILENO, self.buffer.as_mut_ptr().cast(), self.buffer.len()) };
        if count > 0 {
            self.pending_input = Some(self.buffer[..count as usize].to_vec());
            let _ = flush_pending_input(self.pty, &mut self.pending_input)?;
            return Ok(());
        }
        if count == 0 {
            self.pty.write(&[])?;
        }
        self.stdin_open = false;
        Ok(())
    }

    fn drain_output(&mut self) -> std::io::Result<()> {
        drain_output_to(self.pty, &mut self.buffer, &mut self.output)
    }
}

fn flush_pending_input(pty: &mut dyn hl_ws_term::PtyBackend, pending: &mut Option<Vec<u8>>) -> std::io::Result<bool> {
    let Some(bytes) = pending.as_deref() else {
        return Ok(false);
    };
    match pty.write(bytes) {
        Ok(()) => {
            pending.take();
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error),
    }
}

fn drain_output_to(
    pty: &mut dyn hl_ws_term::PtyBackend,
    buffer: &mut [u8; 8192],
    output: &mut impl Write,
) -> std::io::Result<()> {
    let mut wrote = false;
    let mut drained = 0;
    loop {
        let count = pty.read(buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        wrote = true;
        drained += count;
        if drained >= OUTPUT_DRAIN_BUDGET {
            break;
        }
    }
    if wrote {
        output.flush()?;
    }
    Ok(())
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
    let _ = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut nofile) };
    let mut environment: Vec<_> = std::env::vars().collect();
    environment.sort_by(|left, right| left.0.cmp(&right.0));
    format!(
        "terminal tty={terminal} sid={} pgid={} foreground={} size={:?} cwd={} nofile={}/{} descriptors=[{}] environment={environment:?}",
        unsafe { libc::getsid(0) },
        unsafe { libc::getpgrp() },
        unsafe { libc::tcgetpgrp(descriptor) },
        size(),
        std::env::current_dir().map_or_else(|_| "?".to_owned(), |path| path.to_string_lossy().into_owned()),
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
                    .map_or_else(|_| "?".to_owned(), |path| path.to_string_lossy().into_owned());
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
        let mut terminal = TerminalSession::new(&mut backend, None);

        let error = terminal.drain_output().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    }

    #[test]
    fn productive_output_yields_to_the_relay_loop() {
        let expected_reads = super::OUTPUT_DRAIN_BUDGET / 8192;
        let mut backend = ProductiveBackend {
            reads: 0,
            available_reads: expected_reads + 1,
        };
        let mut buffer = [0; 8192];

        super::drain_output_to(&mut backend, &mut buffer, &mut io::sink()).unwrap();

        assert_eq!(backend.reads, expected_reads);
    }

    /// The session half of the contract: once raw mode is in effect a Ctrl-C is data, and the
    /// relay must forward that byte to the guest exactly as it does today.
    #[test]
    fn interrupts_typed_after_raw_mode_reach_the_guest_as_the_interrupt_byte() {
        let mut backend = BackpressuredBackend {
            reject_next: false,
            writes: Vec::new(),
        };
        let mut pending = Some(vec![0x03]);

        assert!(!super::flush_pending_input(&mut backend, &mut pending).unwrap());

        assert!(pending.is_none());
        assert_eq!(backend.writes, [vec![0x03]]);
    }

    #[test]
    fn saturated_input_is_retained_until_the_backend_accepts_it() {
        let mut backend = BackpressuredBackend {
            reject_next: true,
            writes: Vec::new(),
        };
        let mut pending = Some(b"large paste".to_vec());

        assert!(super::flush_pending_input(&mut backend, &mut pending).unwrap());
        assert_eq!(pending.as_deref(), Some(b"large paste".as_slice()));
        assert!(backend.writes.is_empty());

        assert!(!super::flush_pending_input(&mut backend, &mut pending).unwrap());
        assert!(pending.is_none());
        assert_eq!(backend.writes, [b"large paste".to_vec()]);
    }

    struct BackpressuredBackend {
        reject_next: bool,
        writes: Vec<Vec<u8>>,
    }

    impl PtyBackend for BackpressuredBackend {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.reject_next {
                self.reject_next = false;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.writes.push(bytes.to_vec());
            Ok(())
        }

        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn resize(&mut self, _columns: u16, _rows: u16) {}

        fn master_fd(&self) -> Option<RawFd> {
            None
        }

        fn try_wait(&mut self) -> Option<i32> {
            None
        }
    }

    struct ProductiveBackend {
        reads: usize,
        available_reads: usize,
    }

    impl PtyBackend for ProductiveBackend {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<()> {
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads > self.available_reads {
                return Ok(0);
            }
            buffer.fill(b'x');
            Ok(buffer.len())
        }

        fn resize(&mut self, _columns: u16, _rows: u16) {}

        fn master_fd(&self) -> Option<RawFd> {
            None
        }

        fn try_wait(&mut self) -> Option<i32> {
            None
        }
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
