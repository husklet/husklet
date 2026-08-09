// The pty allocation and `posix_spawn` calls in this host adapter are `unsafe` libc entry points.
#![allow(unsafe_code)]

use crate::*;

pub(crate) struct PtyProcess;

impl PtyProcess {
    pub(crate) fn spawn(term: &vte4::Terminal, argv: &[&str], env: &[&str]) -> std::io::Result<(i32, vte4::Pty)> {
        use std::ffi::{CStr, CString};
        use std::os::fd::FromRawFd as _;
        // Darwin and glibc assign different values to the non-portable SETSID extension. Resetting the
        // mask/dispositions keeps GTK's process-wide signal policy out of the isolated worker.
        #[cfg(target_os = "macos")]
        const POSIX_SPAWN_FLAGS: libc::c_short = 0x0400 | 0x4000 | 0x0004 | 0x0008;
        #[cfg(target_os = "linux")]
        const POSIX_SPAWN_FLAGS: libc::c_short = 0x0080 | 0x0004 | 0x0008;
        let c_argv = Self::strings(argv, "argument")?;
        if c_argv.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "terminal process requires an executable",
            ));
        }
        let c_env = Self::strings(env, "environment value")?;
        // SAFETY: every pointer handed to the pty and spawn calls names storage owned by this frame and outliving the call.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if master < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                libc::close(master);
                return Err(std::io::Error::last_os_error());
            }
            // A sane initial winsize so full-screen apps (htop) aren't malformed before the first resize
            // sync; the real size is applied from the terminal grid right after (see the poller below).
            let iws = libc::winsize {
                ws_row: 40,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            libc::ioctl(master, libc::TIOCSWINSZ, &iws);
            let sname = libc::ptsname(master);
            if sname.is_null() {
                libc::close(master);
                return Err(std::io::Error::other("ptsname failed"));
            }
            let slave = CString::from(CStr::from_ptr(sname));

            let mut fa: libc::posix_spawn_file_actions_t = std::mem::zeroed();
            libc::posix_spawn_file_actions_init(&raw mut fa);
            // Open the slave as the child's stdin (no O_NOCTTY → becomes controlling tty for the session
            // leader), then dup to stdout/stderr; close the master in the child.
            libc::posix_spawn_file_actions_addopen(&raw mut fa, 0, slave.as_ptr(), libc::O_RDWR, 0);
            libc::posix_spawn_file_actions_adddup2(&raw mut fa, 0, 1);
            libc::posix_spawn_file_actions_adddup2(&raw mut fa, 0, 2);
            libc::posix_spawn_file_actions_addclose(&raw mut fa, master);

            let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
            libc::posix_spawnattr_init(&raw mut attr);
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut mask);
            libc::posix_spawnattr_setsigmask(&raw mut attr, &raw const mask);
            let mut defaults: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut defaults);
            // Signals 1..31 are the portable POSIX/BSD set that a GUI runtime may alter. Real-time
            // signals are platform-specific and remain untouched.
            for signal in 1..=31 {
                if signal != libc::SIGKILL && signal != libc::SIGSTOP {
                    libc::sigaddset(&raw mut defaults, signal);
                }
            }
            libc::posix_spawnattr_setsigdefault(&raw mut attr, &raw const defaults);
            // macOS GUI libraries keep internal descriptors without FD_CLOEXEC. The Darwin
            // CLOEXEC_DEFAULT extension (0x4000) closes every descriptor not mentioned by these file
            // actions, preventing GTK event pipes/sockets from leaking into the engine worker.
            libc::posix_spawnattr_setflags(&raw mut attr, POSIX_SPAWN_FLAGS);

            let mut p_argv: Vec<*mut libc::c_char> = c_argv.iter().map(|c| c.as_ptr().cast_mut()).collect();
            p_argv.push(std::ptr::null_mut());
            let mut p_env: Vec<*mut libc::c_char> = c_env.iter().map(|c| c.as_ptr().cast_mut()).collect();
            p_env.push(std::ptr::null_mut());

            let mut pid: libc::pid_t = 0;
            let rc = libc::posix_spawn(
                &raw mut pid,
                p_argv[0],
                &raw const fa,
                &raw const attr,
                p_argv.as_ptr(),
                p_env.as_ptr(),
            );
            libc::posix_spawn_file_actions_destroy(&raw mut fa);
            libc::posix_spawnattr_destroy(&raw mut attr);
            if rc != 0 {
                libc::close(master);
                return Err(std::io::Error::from_raw_os_error(rc));
            }

            // Give the master to VTE (it takes ownership and drives the grid + resizes the tty).
            let owned = std::os::fd::OwnedFd::from_raw_fd(master);
            let pty = vte4::Pty::foreign_sync(owned, gio::Cancellable::NONE)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            term.set_pty(Some(&pty));
            Ok((pid, pty))
        }
    }

    fn strings(values: &[&str], kind: &str) -> std::io::Result<Vec<std::ffi::CString>> {
        values
            .iter()
            .map(|value| {
                std::ffi::CString::new(*value).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("terminal {kind} contains a NUL byte"),
                    )
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::PtyProcess;

    #[test]
    fn nul_bytes_are_rejected_before_allocating_a_pty() {
        let error = PtyProcess::strings(&["valid", "bad\0value"], "argument").unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("NUL"));
    }
}
