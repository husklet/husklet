use crate::*;

pub(crate) struct PtyProcess;

impl PtyProcess {
    pub(crate) fn spawn(
        term: &vte4::Terminal,
        argv: &[&str],
        env: &[&str],
    ) -> std::io::Result<(i32, vte4::Pty)> {
        use std::ffi::{CStr, CString};
        // Darwin and glibc assign different values to the non-portable SETSID extension.
        #[cfg(target_os = "macos")]
        const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;
        #[cfg(target_os = "linux")]
        const POSIX_SPAWN_SETSID: libc::c_short = 0x0080;
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
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "ptsname failed",
                ));
            }
            let slave = CString::from(CStr::from_ptr(sname));

            let mut fa: libc::posix_spawn_file_actions_t = std::mem::zeroed();
            libc::posix_spawn_file_actions_init(&mut fa);
            // Open the slave as the child's stdin (no O_NOCTTY → becomes controlling tty for the session
            // leader), then dup to stdout/stderr; close the master in the child.
            libc::posix_spawn_file_actions_addopen(&mut fa, 0, slave.as_ptr(), libc::O_RDWR, 0);
            libc::posix_spawn_file_actions_adddup2(&mut fa, 0, 1);
            libc::posix_spawn_file_actions_adddup2(&mut fa, 0, 2);
            libc::posix_spawn_file_actions_addclose(&mut fa, master);

            let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
            libc::posix_spawnattr_init(&mut attr);
            libc::posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETSID);

            let c_argv: Vec<CString> = argv.iter().map(|s| CString::new(*s).unwrap()).collect();
            let mut p_argv: Vec<*mut libc::c_char> =
                c_argv.iter().map(|c| c.as_ptr() as *mut _).collect();
            p_argv.push(std::ptr::null_mut());
            let c_env: Vec<CString> = env.iter().map(|s| CString::new(*s).unwrap()).collect();
            let mut p_env: Vec<*mut libc::c_char> =
                c_env.iter().map(|c| c.as_ptr() as *mut _).collect();
            p_env.push(std::ptr::null_mut());

            let mut pid: libc::pid_t = 0;
            let rc = libc::posix_spawn(
                &mut pid,
                p_argv[0],
                &fa,
                &attr,
                p_argv.as_ptr(),
                p_env.as_ptr(),
            );
            libc::posix_spawn_file_actions_destroy(&mut fa);
            libc::posix_spawnattr_destroy(&mut attr);
            if rc != 0 {
                libc::close(master);
                return Err(std::io::Error::from_raw_os_error(rc));
            }

            // Give the master to VTE (it takes ownership and drives the grid + resizes the tty).
            use std::os::fd::FromRawFd;
            let owned = std::os::fd::OwnedFd::from_raw_fd(master);
            let pty = vte4::Pty::foreign_sync(owned, gio::Cancellable::NONE)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
            term.set_pty(Some(&pty));
            Ok((pid, pty))
        }
    }
}
