//! `LocalPty` — a shell on a real host PTY via the POSIX-98 pty API (`posix_openpt`/`grantpt`/
//! `unlockpt`/`ptsname`) + `fork`/`execvp`. Portable across Linux and macOS with no extra link libs.
//!
//! This is what makes the whole terminal headlessly testable: spawn a real `bash`, drive its stdin,
//! read its output off the master fd, feed it to [`crate::Vt`], and assert the resulting grid — no GPU,
//! no display, no container engine. On macOS the same code also underlies "local shell" workspaces; the
//! in-container workspace path is the sibling `DdJitPty`.

use super::PtyBackend;
use std::ffi::{CString, CStr};
use std::io;
use std::os::unix::io::RawFd;

/// A forked child shell attached to a PTY master fd.
pub struct LocalPty {
    master: RawFd,
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
        // Marshal argv + env into C strings BEFORE fork (allocation after fork is unsafe).
        let c_args: Vec<CString> = argv
            .iter()
            .map(|a| CString::new(*a).unwrap_or_default())
            .collect();
        let mut c_argv: Vec<*const libc::c_char> = c_args.iter().map(|a| a.as_ptr()).collect();
        c_argv.push(std::ptr::null());
        let c_env: Vec<(CString, CString)> = env
            .iter()
            .map(|(k, v)| (CString::new(*k).unwrap_or_default(), CString::new(*v).unwrap_or_default()))
            .collect();

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
            let slave_path = CStr::from_ptr(sname).to_owned();
            // Seed the window size on the master so TUIs see a sane size before the first resize.
            let ws = libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };
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
                for (k, v) in &c_env {
                    libc::setenv(k.as_ptr(), v.as_ptr(), 1);
                }
                libc::execvp(c_argv[0], c_argv.as_ptr());
                libc::_exit(127); // exec failed
            }

            // ---- PARENT ----
            let flags = libc::fcntl(master, libc::F_GETFL, 0);
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
            Ok(LocalPty { master, child: pid, exited: None })
        }
    }
}

impl PtyBackend for LocalPty {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let n = unsafe {
                libc::write(self.master, bytes.as_ptr() as *const libc::c_void, bytes.len())
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
            libc::read(self.master, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
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
        let ws = libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };
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
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let n = pty.read(&mut buf).unwrap_or(0);
            if n > 0 {
                vt.advance_bytes(&buf[..n]);
                continue;
            }
            if pty.try_wait().is_some() {
                // Child exited; drain any final buffered output, then stop.
                loop {
                    let n = pty.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    vt.advance_bytes(&buf[..n]);
                }
                break;
            }
            if Instant::now() > deadline {
                panic!("pty test timed out");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        vt
    }

    fn have(prog: &str) -> bool {
        std::path::Path::new(prog).exists()
    }

    #[test]
    fn real_shell_prints_text_through_the_vt() {
        // The end-to-end headless proof: a real shell on a real PTY -> our VT parser -> grid.
        let sh = if have("/bin/bash") { "/bin/bash" } else { "/bin/sh" };
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
        let sh = if have("/bin/bash") { "/bin/bash" } else { "/bin/sh" };
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
        let cat = if have("/bin/cat") { "/bin/cat" } else { "/usr/bin/cat" };
        let mut pty = LocalPty::spawn(&[cat], 40, 5, &[("TERM", "xterm-256color")])
            .expect("spawn cat");
        pty.write(b"ping\n").unwrap();
        // Read the echo (cat + tty echo), then close stdin so cat exits.
        std::thread::sleep(Duration::from_millis(50));
        let mut vt = Vt::new(40, 5);
        let mut buf = [0u8; 1024];
        let n = pty.read(&mut buf).unwrap_or(0);
        vt.advance_bytes(&buf[..n]);
        assert!(vt.grid().row_text(0).contains("ping"), "got {:?}", vt.grid().row_text(0));
    }
}
