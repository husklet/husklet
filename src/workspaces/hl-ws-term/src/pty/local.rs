//! `LocalPty` — a shell on a real host PTY via the POSIX-98 pty API (`posix_openpt`/`grantpt`/
//! `unlockpt`/`ptsname`) + `fork`/`execvp`. Portable across Linux and macOS with no extra link libs.
//!
//! This is what makes the whole terminal headlessly testable: spawn a real `bash`, drive its stdin,
//! read its output off the master fd, feed it to [`crate::Vt`], and assert the resulting grid — no GPU,
//! no display, no container engine. On macOS the same code also underlies "local shell" workspaces; the
//! in-container workspace path is the sibling `HlJitPty`.

use super::PtyBackend;
use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::io::RawFd;

/// Resolve a program name to an absolute path (done in the parent, before fork). If it already contains
/// a `/`, it is used as-is; otherwise `PATH` is searched for the first executable match.
fn resolve_prog(prog: &str) -> Option<String> {
    if prog.contains('/') {
        return Some(prog.to_string());
    }
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = std::path::Path::new(dir).join(prog);
        if let Ok(c) = CString::new(cand.to_string_lossy().as_bytes()) {
            if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

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
    pub fn spawn(
        argv: &[&str],
        cols: u16,
        rows: u16,
        env: &[(&str, &str)],
    ) -> io::Result<LocalPty> {
        if argv.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
        }
        // Everything the child needs is built in the PARENT and passed to a single `execve`. Between
        // `fork` and `exec` the child does NO locking/allocating call (no `setenv`, no PATH search, no
        // `execvp`) — those take locks that a *concurrent* thread may hold at fork time, which would
        // deadlock the child (the classic fork-in-a-threaded-program hazard). Resolve the program path
        // and merge the environment here, up front.
        let prog = resolve_prog(argv[0])
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "program not found on PATH"))?;
        let c_prog = CString::new(prog).unwrap_or_default();
        let c_args: Vec<CString> = argv
            .iter()
            .map(|a| CString::new(*a).unwrap_or_default())
            .collect();
        let mut c_argv: Vec<*const libc::c_char> = c_args.iter().map(|a| a.as_ptr()).collect();
        c_argv.push(std::ptr::null());
        // Merge the process environment with the caller's overrides into a full `KEY=VAL` envp.
        let mut merged: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        for (k, v) in env {
            merged.insert((*k).to_string(), (*v).to_string());
        }
        let c_envs: Vec<CString> = merged
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
        static PTY_ALLOC: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let (master, slave_path) = {
            let _guard = PTY_ALLOC.lock().unwrap_or_else(|p| p.into_inner());
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
}

impl PtyBackend for LocalPty {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let n = unsafe {
                libc::write(
                    self.master,
                    bytes.as_ptr() as *const libc::c_void,
                    bytes.len(),
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(libc::EINTR) {
                    continue; // transient: retry
                }
                return Err(e);
            }
            bytes = &bytes[n as usize..];
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe {
            libc::read(
                self.master,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
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
        unsafe {
            libc::ioctl(self.master, libc::TIOCSWINSZ, &ws);
        }
    }

    fn master_fd(&self) -> Option<RawFd> {
        Some(self.master)
    }

    fn try_wait(&mut self) -> Option<i32> {
        if self.exited.is_some() {
            return self.exited;
        }
        let mut status: libc::c_int = 0;
        let r = unsafe { libc::waitpid(self.child, &mut status, libc::WNOHANG) };
        if r == self.child {
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

impl Drop for LocalPty {
    fn drop(&mut self) {
        unsafe {
            if self.exited.is_none() {
                libc::kill(self.child, libc::SIGHUP);
                // Best-effort reap so we don't leak a zombie.
                let mut status = 0;
                libc::waitpid(self.child, &mut status, libc::WNOHANG);
            }
            if self.slave >= 0 {
                libc::close(self.slave);
            }
            libc::close(self.master);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vt;
    use std::time::{Duration, Instant};

    /// Drive a `LocalPty` to completion, feeding all output into a fresh `Vt`, and return it.
    /// Blocks (with a timeout) until the child exits and the master drains — a synchronous helper for
    /// deterministic tests. Real GUI use polls the `master_fd` on an event loop instead.
    fn run_into_vt(mut pty: LocalPty, cols: usize, rows: usize) -> Vt {
        let mut vt = Vt::new(cols, rows);
        let mut buf = [0u8; 4096];
        let fd = pty.master_fd().unwrap();
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
            let pr = unsafe { libc::poll(&mut pfd, 1, 20) };
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
            if Instant::now() > deadline {
                panic!("pty test timed out");
            }
        }
        vt
    }

    fn have(prog: &str) -> bool {
        std::path::Path::new(prog).exists()
    }

    #[test]
    fn real_shell_prints_text_through_the_vt() {
        // The end-to-end headless proof: a real shell on a real PTY -> our VT parser -> grid.
        let sh = if have("/bin/bash") {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        let pty = LocalPty::spawn(
            &[sh, "-c", "printf 'hello\\r\\n'; printf 'world\\r\\n'"],
            40,
            10,
            &[("TERM", "xterm-256color")],
        )
        .expect("spawn shell");
        let vt = run_into_vt(pty, 40, 10);
        assert_eq!(vt.grid().row_text(0), "hello");
        assert_eq!(vt.grid().row_text(1), "world");
    }

    #[test]
    fn real_shell_ansi_color_reaches_the_grid() {
        let sh = if have("/bin/bash") {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        let pty = LocalPty::spawn(
            &[sh, "-c", "printf '\\033[31mRED\\033[0m'"],
            40,
            5,
            &[("TERM", "xterm-256color")],
        )
        .expect("spawn shell");
        let vt = run_into_vt(pty, 40, 5);
        assert_eq!(vt.grid().row_text(0), "RED");
        assert_eq!(vt.grid().cell(0, 0).unwrap().fg, crate::Color::Indexed(1));
    }

    #[test]
    fn write_to_shell_stdin_is_echoed_back() {
        // `cat` echoes stdin; prove the write path reaches the child and its output returns.
        let cat = if have("/bin/cat") {
            "/bin/cat"
        } else {
            "/usr/bin/cat"
        };
        let mut pty =
            LocalPty::spawn(&[cat], 40, 5, &[("TERM", "xterm-256color")]).expect("spawn cat");
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
