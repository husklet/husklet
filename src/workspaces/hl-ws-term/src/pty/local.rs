//! `LocalPty` — a shell on a real host PTY via the POSIX-98 pty API (`posix_openpt`/`grantpt`/
//! `unlockpt`/`ptsname`) + `fork`/`execvp`. Portable across Linux and macOS with no extra link libs.
//!
//! This is what makes the whole terminal headlessly testable: spawn a real `bash`, drive its stdin,
//! read its output off the master fd, feed it to [`crate::Vt`], and assert the resulting grid — no GPU,
//! no display, no container engine. On macOS the same code also underlies "local shell" workspaces; the
//! in-container workspace path is the sibling `HlJitPty`.

// The POSIX pty and fork/exec calls this module is made of are all `unsafe` libc entry points.
#![allow(unsafe_code)]

use super::PtyBackend;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::io::RawFd;

const PTY_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// A forked child shell attached to a PTY master fd.
pub struct LocalPty {
    master: RawFd,
    /// A parent-held copy of the slave fd. Keeping it open means the master never sees EIO/EOF when the
    /// child exits, so the kernel does NOT discard still-buffered output — we drain it reliably and use
    /// `waitpid` (not master EOF) to detect exit. Avoids the Linux "fast writer + exit loses data" race.
    slave: RawFd,
    child: libc::pid_t,
    exited: Option<i32>,
}

impl LocalPty {
    /// Fork `argv` on a fresh PTY sized `cols × rows`. `argv[0]` should be an absolute path or a name on
    /// `PATH` (resolved by `execvp`). `env` entries are `setenv`'d in the child before exec.
    pub fn spawn(argv: &[&str], cols: u16, rows: u16, env: &BTreeMap<String, String>) -> io::Result<LocalPty> {
        if argv.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
        }
        // Everything the child needs is built in the PARENT and passed to a single `execve`. Between
        // `fork` and `exec` the child does NO locking/allocating call (no `setenv`, no PATH search, no
        // `execvp`) — those take locks that a *concurrent* thread may hold at fork time, which would
        // deadlock the child (the classic fork-in-a-threaded-program hazard). Resolve the program path
        // and merge the environment here, up front.
        if !argv[0].contains('/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "executable path must be explicit",
            ));
        }
        let prog = argv[0];
        let c_prog = CString::new(prog).unwrap_or_default();
        let c_args: Vec<CString> = argv.iter().map(|a| CString::new(*a).unwrap_or_default()).collect();
        let mut c_argv: Vec<*const libc::c_char> = c_args.iter().map(|a| a.as_ptr()).collect();
        c_argv.push(std::ptr::null());
        // Merge the process environment with the caller's overrides into a full `KEY=VAL` envp.
        let c_envs: Vec<CString> = env
            .iter()
            .map(|(k, v)| CString::new(format!("{k}={v}")).unwrap_or_default())
            .collect();
        let mut c_envp: Vec<*const libc::c_char> = c_envs.iter().map(|e| e.as_ptr()).collect();
        c_envp.push(std::ptr::null());

        // `ptsname` returns a pointer into a single STATIC buffer, so two threads allocating PTYs at
        // once race on it (→ children open the wrong/garbage slave path, losing all their output). Guard
        // the whole allocate-and-read-name critical section with a process-wide lock, copying the name
        // into owned storage before releasing it. (`ptsname_r` would avoid this but isn't portable to
        // macOS.) The lock is released before `fork` so the child never inherits a held lock.
        let (master, slave_path) = {
            let _guard = PTY_ALLOC.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // SAFETY: the block only names the master descriptor it just opened, and the `ptsname` buffer is copied while the allocation lock is still held.
            unsafe {
                let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
                if master < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                    let e = io::Error::last_os_error();
                    libc::close(master);
                    return Err(e);
                }
                let sname = libc::ptsname(master);
                if sname.is_null() {
                    let e = io::Error::last_os_error();
                    libc::close(master);
                    return Err(e);
                }
                (master, CStr::from_ptr(sname).to_owned())
            }
        };

        // SAFETY: `ws` is a live initialised aggregate, and the fork child only calls async-signal-safe entry points before `execvp`.
        unsafe {
            // Seed the window size on the master so TUIs see a sane size before the first resize.
            let ws = libc::winsize {
                ws_row: rows.max(1),
                ws_col: cols.max(1),
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            libc::ioctl(master, libc::TIOCSWINSZ, &ws);

            let pid = libc::fork();
            if pid < 0 {
                let e = io::Error::last_os_error();
                libc::close(master);
                return Err(e);
            }
            if pid == 0 {
                // ---- CHILD: become a session leader with the slave as controlling terminal ----
                libc::close(master);
                libc::setsid();
                let slave = libc::open(slave_path.as_ptr(), libc::O_RDWR);
                if slave < 0 {
                    libc::_exit(127);
                }
                // Make the slave our controlling tty (the `0` arg is the "steal" flag).
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0 as libc::c_int);
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);
                if slave > 2 {
                    libc::close(slave);
                }
                // async-signal-safe: a single execve with the parent-built argv + envp. No locks.
                libc::execve(c_prog.as_ptr(), c_argv.as_ptr(), c_envp.as_ptr());
                libc::_exit(127); // exec failed
            }

            // ---- PARENT ----
            // Hold our own copy of the slave open (O_NOCTTY so it doesn't steal our controlling tty).
            let slave = libc::open(slave_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
            let flags = libc::fcntl(master, libc::F_GETFL, 0);
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
            Ok(LocalPty {
                master,
                slave,
                child: pid,
                exited: None,
            })
        }
    }

    fn close_process_group(&mut self) {
        if self.exited.is_some() && !self.process_group_exists() {
            return;
        }
        self.signal_process_group(libc::SIGHUP);
        let deadline = std::time::Instant::now() + PTY_CLOSE_GRACE;
        while std::time::Instant::now() < deadline {
            self.reap_child();
            if self.exited.is_some() && !self.process_group_exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if self.exited.is_none() || self.process_group_exists() {
            self.signal_process_group(libc::SIGKILL);
        }
        if self.exited.is_none() {
            let mut status = 0;
            // SAFETY: `child` is the sole child process owned by this PTY. A blocking wait after
            // SIGKILL reaps it exactly once and retains no pointer to `status`.
            let waited = unsafe { libc::waitpid(self.child, &raw mut status, 0) };
            if waited == self.child {
                self.exited = Some(Self::exit_code(status));
            }
        }
    }

    fn signal_process_group(&self, signal: libc::c_int) {
        // The child calls setsid, so its PID becomes its process-group ID. Deliver to both the group and
        // an unreaped leader: the positive target closes the short race where Drop runs before setsid
        // completes. Once reaped, the positive PID could have been reused and must not be signalled.
        // SAFETY: the calls consume integer process identities and retain no Rust storage.
        unsafe {
            libc::kill(-self.child, signal);
            if self.exited.is_none() {
                libc::kill(self.child, signal);
            }
        }
    }

    fn process_group_exists(&self) -> bool {
        // SAFETY: signal zero probes the process-group identity without delivering a signal.
        unsafe { libc::kill(-self.child, 0) == 0 }
    }

    fn reap_child(&mut self) -> bool {
        let mut status = 0;
        // SAFETY: `status` is live writable storage and `child` is owned by this PTY.
        let waited = unsafe { libc::waitpid(self.child, &raw mut status, libc::WNOHANG) };
        if waited == self.child {
            self.exited = Some(Self::exit_code(status));
            true
        } else {
            false
        }
    }

    fn exit_code(status: libc::c_int) -> i32 {
        if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            128 + libc::WTERMSIG(status)
        } else {
            -1
        }
    }
}

impl PtyBackend for LocalPty {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            // SAFETY: the source pointer and length come from the same live borrowed slice.
            let n = unsafe { libc::write(self.master, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
            if n >= 0 {
                bytes = &bytes[n as usize..];
                continue;
            }
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(libc::EINTR) {
                continue; // transient: retry
            }
            return Err(e);
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: the destination pointer and length come from the same live mutable slice.
        let n = unsafe { libc::read(self.master, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EAGAIN) => Ok(0), // nothing available right now
            // Linux returns EIO on the master once the slave side is fully closed (child exited).
            Some(libc::EIO) => Ok(0),
            Some(libc::EINTR) => Ok(0),
            _ => Err(e),
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows.max(1),
            ws_col: cols.max(1),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `ws` is a live initialised aggregate borrowed for the duration of the call.
        unsafe {
            libc::ioctl(self.master, libc::TIOCSWINSZ, &ws);
        }
    }

    fn master_descriptor(&self) -> Option<RawFd> {
        Some(self.master)
    }

    fn try_wait(&mut self) -> Option<i32> {
        if self.exited.is_some() {
            return self.exited;
        }
        let mut status: libc::c_int = 0;
        // SAFETY: `status` is a live local the kernel writes exclusively for this call.
        let r = unsafe { libc::waitpid(self.child, &raw mut status, libc::WNOHANG) };
        if r == self.child {
            self.exited = Some(Self::exit_code(status));
        }
        self.exited
    }
}

impl Drop for LocalPty {
    fn drop(&mut self) {
        self.close_process_group();
        // SAFETY: the descriptors and child pid are owned by this value and still valid while it drops.
        unsafe {
            if self.slave >= 0 {
                libc::close(self.slave);
            }
            libc::close(self.master);
        }
    }
}
static PTY_ALLOC: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vt;
    use std::time::{Duration, Instant};

    /// Drive a `LocalPty` to completion, feeding all output into a fresh `Vt`, and return it.
    /// Blocks (with a timeout) until the child exits and the master drains — a synchronous helper for
    /// deterministic tests. Real GUI use polls the `master_descriptor` on an event loop instead.
    fn run_into_vt(mut pty: LocalPty, cols: usize, rows: usize) -> Vt {
        let mut vt = Vt::new(cols, rows);
        let mut buf = [0u8; 4096];
        let fd = pty.master_descriptor().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        loop {
            // Block up to 20ms for readability instead of a fixed sleep — no race window where the child
            // writes-and-exits between two polls. Because LocalPty holds the slave open, exit never
            // discards buffered output, so after `waitpid` an empty poll means truly drained.
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is a live, fully initialised `pollfd` owned by this frame.
            let pr = unsafe { libc::poll(&raw mut pfd, 1, 20) };
            if pr > 0 && pfd.revents & libc::POLLIN != 0 {
                let n = pty.read(&mut buf).unwrap_or(0);
                if n > 0 {
                    vt.advance_bytes(&buf[..n]);
                    continue;
                }
            }
            if exited {
                break; // child gone and nothing left readable
            }
            if pty.try_wait().is_some() {
                exited = true;
                continue; // loop once more to drain the fully-buffered final output
            }
            assert!(Instant::now() <= deadline, "pty test timed out");
        }
        vt
    }

    fn have(prog: &str) -> bool {
        std::path::Path::new(prog).exists()
    }

    fn read_process_id(pty: &mut LocalPty) -> libc::pid_t {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 64];
        while Instant::now() < deadline {
            if let Ok(count) = pty.read(&mut buffer) {
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.contains(&b'\n') {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        String::from_utf8_lossy(&bytes)
            .trim()
            .parse()
            .expect("shell must report its descendant pid")
    }

    fn process_exists(process: libc::pid_t) -> bool {
        // SAFETY: signal zero only probes the integer process identity.
        unsafe { libc::kill(process, 0) == 0 }
    }

    fn pre_session_ignored_hangup_child() -> libc::pid_t {
        let mut ready = [-1; 2];
        // SAFETY: the pipe array is valid writable storage for two descriptors.
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
        let shell = std::ffi::CString::new("/bin/sh").unwrap();
        let name = std::ffi::CString::new("sh").unwrap();
        let option = std::ffi::CString::new("-c").unwrap();
        let script = std::ffi::CString::new("printf x >&3; exec sleep 2").unwrap();
        // SAFETY: fork creates one owned child. Every child-side operation below is async-signal-safe,
        // and all exec strings were allocated in the parent.
        let child = unsafe { libc::fork() };
        if child == 0 {
            // SAFETY: the child exclusively owns its descriptor table and immediately execs or exits.
            unsafe {
                libc::close(ready[0]);
                libc::dup2(ready[1], 3);
                if ready[1] != 3 {
                    libc::close(ready[1]);
                }
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                libc::execl(
                    shell.as_ptr(),
                    name.as_ptr(),
                    option.as_ptr(),
                    script.as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
                libc::_exit(127);
            }
        }
        assert!(child > 1, "fork test child");
        // SAFETY: only the child needs the write end after fork.
        unsafe { libc::close(ready[1]) };
        let mut poll = libc::pollfd {
            fd: ready[0],
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the parent owns the live pipe descriptor and `poll` is writable for this call.
        let readable = unsafe { libc::poll(&raw mut poll, 1, 2_000) };
        if readable != 1 {
            // SAFETY: this failure path owns the child and remaining read descriptor.
            unsafe {
                libc::kill(child, libc::SIGKILL);
                libc::waitpid(child, std::ptr::null_mut(), 0);
                libc::close(ready[0]);
            }
            panic!("pre-session child did not exec within two seconds");
        }
        // SAFETY: the parent owns these pipe descriptors and reads one readiness byte before closing.
        unsafe {
            let mut byte = 0_u8;
            assert_eq!(libc::read(ready[0], (&raw mut byte).cast(), 1), 1);
            libc::close(ready[0]);
        }
        child
    }

    #[test]
    fn real_shell_prints_text_through_the_vt() {
        // The end-to-end headless proof: a real shell on a real PTY -> our VT parser -> grid.
        let sh = if have("/bin/bash") { "/bin/bash" } else { "/bin/sh" };
        let pty = LocalPty::spawn(
            &[sh, "-c", "printf 'hello\\r\\n'; printf 'world\\r\\n'"],
            40,
            10,
            &std::collections::BTreeMap::from([(String::from("TERM"), String::from("xterm-256color"))]),
        )
        .expect("spawn shell");
        let vt = run_into_vt(pty, 40, 10);
        assert_eq!(vt.grid().row_text(0), "hello");
        assert_eq!(vt.grid().row_text(1), "world");
    }

    #[test]
    fn dropping_a_pty_reaps_its_hup_ignoring_process_group() {
        let mut pty = LocalPty::spawn(
            &[
                "/bin/sh",
                "-c",
                "trap '' HUP TERM; sleep 60 & printf '%s\\n' \"$!\"; wait",
            ],
            40,
            10,
            &std::collections::BTreeMap::new(),
        )
        .expect("spawn process tree");
        let descendant = read_process_id(&mut pty);
        assert!(process_exists(descendant));

        drop(pty);
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_exists(descendant) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let survived = process_exists(descendant);
        if survived {
            // Never leak the mutation's deliberately surviving process into another test or lane.
            // SAFETY: this test exclusively owns the reported descendant process identity.
            unsafe { libc::kill(descendant, libc::SIGKILL) };
        }
        assert!(!survived, "PTY descendant survived its owner's bounded teardown");
    }

    #[test]
    fn dropping_before_setsid_forces_a_leader_inheriting_ignored_hangup() {
        let child = pre_session_ignored_hangup_child();
        // Readiness is written after exec, while the child deliberately remains in this test's process
        // group. It has therefore inherited SIG_IGN across exec but has not reached the PTY setsid state.
        // SAFETY: getpgid only reads the live child process identity.
        assert_ne!(unsafe { libc::getpgid(child) }, child);
        // LocalPty closes both descriptors after owning and reaping the child.
        // SAFETY: each open returns a new descriptor owned by the constructed LocalPty.
        let master = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        // SAFETY: as above, this is a second independently owned descriptor.
        let slave = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        assert!(master >= 0 && slave >= 0);
        let pty = LocalPty {
            master,
            slave,
            child,
            exited: None,
        };

        let started = Instant::now();
        drop(pty);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!process_exists(child));
    }

    #[test]
    fn dropping_a_reaped_pty_leader_still_reaps_its_process_group() {
        let mut pty = LocalPty::spawn(
            &[
                "/bin/sh",
                "-c",
                "trap '' HUP TERM; sleep 60 & printf '%s\\n' \"$!\"; exit 0",
            ],
            40,
            10,
            &std::collections::BTreeMap::new(),
        )
        .expect("spawn detached descendant");
        let descendant = read_process_id(&mut pty);
        let deadline = Instant::now() + Duration::from_secs(2);
        while pty.try_wait().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(pty.exited, Some(0), "PTY leader must be reaped before ownership drops");
        assert!(process_exists(descendant));

        drop(pty);
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_exists(descendant) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let survived = process_exists(descendant);
        if survived {
            // SAFETY: this test exclusively owns the reported descendant process identity.
            unsafe { libc::kill(descendant, libc::SIGKILL) };
        }
        assert!(!survived, "reaped PTY leader stranded its process group");
    }

    #[test]
    fn real_shell_ansi_color_reaches_the_grid() {
        let sh = if have("/bin/bash") { "/bin/bash" } else { "/bin/sh" };
        let pty = LocalPty::spawn(
            &[sh, "-c", "printf '\\033[31mRED\\033[0m'"],
            40,
            5,
            &std::collections::BTreeMap::from([(String::from("TERM"), String::from("xterm-256color"))]),
        )
        .expect("spawn shell");
        let vt = run_into_vt(pty, 40, 5);
        assert_eq!(vt.grid().row_text(0), "RED");
        assert_eq!(vt.grid().cell(0, 0).unwrap().fg, crate::Color::Indexed(1));
    }

    #[test]
    fn real_shell_renders_a_png() {
        let shell = if have("/bin/bash") { "/bin/bash" } else { "/bin/sh" };
        let pty = LocalPty::spawn(
            &[shell, "-c", "printf '\\033[32mterminal-pipeline\\033[0m\\r\\n'"],
            40,
            5,
            &std::collections::BTreeMap::from([(String::from("TERM"), String::from("xterm-256color"))]),
        )
        .expect("spawn shell");

        let terminal = run_into_vt(pty, 40, 5);
        let png = crate::CpuRenderer::default().render_png(terminal.grid());

        assert_eq!(terminal.grid().row_text(0), "terminal-pipeline");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.len() > 100);
    }

    #[test]
    fn write_to_shell_stdin_is_echoed_back() {
        // `cat` echoes stdin; prove the write path reaches the child and its output returns.
        let cat = if have("/bin/cat") { "/bin/cat" } else { "/usr/bin/cat" };
        let mut pty = LocalPty::spawn(
            &[cat],
            40,
            5,
            &std::collections::BTreeMap::from([(String::from("TERM"), String::from("xterm-256color"))]),
        )
        .expect("spawn cat");
        pty.write(b"ping\n").unwrap();
        // Read the echo (cat + tty echo), then close stdin so cat exits.
        std::thread::sleep(Duration::from_millis(50));
        let mut vt = Vt::new(40, 5);
        let mut buf = [0u8; 1024];
        let n = pty.read(&mut buf).unwrap_or(0);
        vt.advance_bytes(&buf[..n]);
        assert!(
            vt.grid().row_text(0).contains("ping"),
            "got {:?}",
            vt.grid().row_text(0)
        );
    }
}
