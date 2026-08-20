// The pty allocation and `posix_spawn` calls in this host adapter are `unsafe` libc entry points.
#![allow(unsafe_code)]

use crate::*;

pub(crate) struct PtyProcess;

impl PtyProcess {
    pub(crate) fn spawn(term: &vte4::Terminal, argv: &[&str], env: &[&str]) -> std::io::Result<(i32, vte4::Pty)> {
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
            let (master, slave, _quiet) = Self::open_master()?;

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

    /// Allocates the pane's pty, sized and with its local echo already silenced, and names its slave.
    ///
    /// Split out from `spawn` so the launch window's line-discipline state is exercised by a test
    /// without a toolkit terminal.
    fn open_master() -> std::io::Result<(libc::c_int, std::ffi::CString, QuietSlave)> {
        // SAFETY: every call below takes integers or storage owned by this frame for the call's
        // duration. None retains a pointer, aliases Rust storage, invokes a callback, or can unwind
        // across the ABI.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if master < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                let failure = std::io::Error::last_os_error();
                libc::close(master);
                return Err(failure);
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
            let name = libc::ptsname(master);
            if name.is_null() {
                libc::close(master);
                return Err(std::io::Error::other("ptsname failed"));
            }
            let slave = std::ffi::CString::from(std::ffi::CStr::from_ptr(name));
            let quiet = QuietSlave::open(&slave);
            Ok((master, slave, quiet))
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

/// Holds the pane's pty in the launch state the not-yet-live banner describes: input queued, no
/// local echo, terminal signals still generated.
///
/// A `posix_openpt` pty arrives **cooked**, and nothing turns its echo off until the pane worker
/// reaches `RawMode::enter` -- which is after the whole launch or restore. Everything typed in that
/// window was therefore drawn twice: once by this line discipline, and again by the guest's own
/// discipline when the shell finally read the queued line.
///
/// Only `ECHO` is cleared. `ICANON` is what holds the typed line in the input queue until the shell
/// starts, which is exactly what the banner promises; `ISIG` is the behaviour `InterruptMask` is
/// written to complement for this same window. Neither is this fix's business.
///
/// Two host facts decide the shape, and both were measured rather than assumed:
///
/// - Darwin refuses `tcgetattr` on a pty **master** with `ENOTTY`, so the attributes are set through
///   the slave. A master-side form of this fix compiles on both hosts and is a no-op on the one the
///   application ships to.
/// - Darwin resets a pty's attributes when its **last** slave descriptor closes, so a set-and-close
///   is undone before `posix_spawn` can open the slave again. This value therefore keeps the slave
///   open across the spawn and closes it afterwards, when the worker's own descriptor is the one
///   holding the pty.
///
/// A pty that cannot report or accept attributes keeps its pane rather than failing the launch, like
/// the winsize: the pane still works, it merely echoes as it did before.
struct QuietSlave(Option<libc::c_int>);

impl QuietSlave {
    fn open(slave: &std::ffi::CStr) -> Self {
        // SAFETY: `slave` is a NUL-terminated path borrowed for the whole call, and `attributes` is
        // an initialized C aggregate owned by this frame and written by the kernel before it is
        // read. None of these calls retains a pointer, aliases Rust storage, invokes a callback, or
        // can unwind across the ABI. `O_CLOEXEC` keeps the descriptor out of the spawned worker,
        // which opens the slave for itself; `O_NOCTTY` keeps it from taking a controlling terminal
        // for the GUI process.
        unsafe {
            let descriptor = libc::open(slave.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            if descriptor < 0 {
                return Self(None);
            }
            let mut attributes: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(descriptor, &raw mut attributes) == 0 {
                attributes.c_lflag &= !libc::ECHO;
                libc::tcsetattr(descriptor, libc::TCSANOW, &raw const attributes);
            }
            Self(Some(descriptor))
        }
    }
}

impl Drop for QuietSlave {
    fn drop(&mut self) {
        let Some(descriptor) = self.0.take() else {
            return;
        };
        // SAFETY: this value exclusively owns the descriptor and is being destroyed.
        unsafe { libc::close(descriptor) };
    }
}

#[cfg(test)]
mod tests {
    use super::PtyProcess;

    /// The typed line, and the marker the stand-in guest wraps its own echo of it in.
    const TYPED: &str = "hi\n";

    struct Master(libc::c_int, std::ffi::CString, Option<super::QuietSlave>);

    impl Master {
        fn open() -> Self {
            let (master, slave, quiet) = PtyProcess::open_master().expect("a pty");
            Self(master, slave, Some(quiet))
        }

        /// What `spawn` does once the worker holds the slave: release the launch-state descriptor.
        fn release_launch_state(&mut self) {
            self.2 = None;
        }

        fn slave_path(&self) -> String {
            self.1.to_string_lossy().into_owned()
        }

        /// What the user types while the pane is not live yet.
        fn type_line(&self, line: &str) {
            // SAFETY: the borrowed bytes outlive the call and the descriptor is open.
            let written = unsafe { libc::write(self.0, line.as_ptr().cast(), line.len()) };
            assert_eq!(written, line.len() as isize, "the typed line reaches the pty");
        }

        /// Everything the master can be read for within `deadline`, as the pane would draw it.
        fn drain(&self, deadline: std::time::Duration) -> String {
            let start = std::time::Instant::now();
            let mut drawn = Vec::new();
            while start.elapsed() < deadline {
                let mut poller = libc::pollfd {
                    fd: self.0,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one initialized descriptor record owned by this frame for the call.
                let ready = unsafe { libc::poll(&raw mut poller, 1, 100) };
                if ready <= 0 {
                    continue;
                }
                let mut chunk = [0_u8; 512];
                // SAFETY: the kernel writes at most `chunk.len()` bytes into storage owned here.
                let read = unsafe { libc::read(self.0, chunk.as_mut_ptr().cast(), chunk.len()) };
                if read <= 0 {
                    // The stand-in guest exited and closed the last slave: EIO ends the pane.
                    break;
                }
                drawn.extend_from_slice(&chunk[..read as usize]);
            }
            String::from_utf8_lossy(&drawn).into_owned()
        }
    }

    impl Drop for Master {
        fn drop(&mut self) {
            // SAFETY: this value exclusively owns the descriptor and is being destroyed.
            unsafe { libc::close(self.0) };
        }
    }

    /// The line-discipline flags in force for the pty, read where both hosts support the question:
    /// Darwin refuses `tcgetattr` on a pty master.
    fn local_flags(slave: &std::fs::File) -> libc::tcflag_t {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `attributes` is an initialized C aggregate owned by this frame and written by the
        // kernel before it is read, over a descriptor the caller keeps open for the call.
        unsafe {
            let mut attributes: libc::termios = std::mem::zeroed();
            assert_eq!(
                libc::tcgetattr(slave.as_raw_fd(), &raw mut attributes),
                0,
                "tcgetattr on the slave"
            );
            attributes.c_lflag
        }
    }

    /// Opens the slave the way `spawn`'s file actions do, before anything can be typed.
    ///
    /// Order matters and is not decoration: on Darwin a pty with no slave open discards what is
    /// written to its master, so a test that types first measures nothing. The product opens the
    /// slave as the worker's stdin at `posix_spawn`, and the launch window the user types into
    /// begins after that.
    fn open_slave(master: &Master) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(master.slave_path())
            .expect("the slave end")
    }

    /// Starts the stand-in for the guest shell: it takes raw mode as the pane worker does, then
    /// echoes the queued line back exactly once, as the guest's own line discipline does.
    fn start_stand_in_guest(slave: std::fs::File) -> std::process::Child {
        std::process::Command::new("sh")
            .arg("-c")
            .arg("stty raw -echo; head -n 1")
            .stdin(slave.try_clone().expect("slave stdin"))
            .stdout(slave.try_clone().expect("slave stdout"))
            .stderr(slave)
            .spawn()
            .expect("the stand-in guest starts")
    }

    /// Ends the stand-in guest by the pid this test started, whatever it did with the line.
    fn stop(mut guest: std::process::Child) {
        let _ = guest.kill();
        let _ = guest.wait();
    }

    #[test]
    fn nul_bytes_are_rejected_before_allocating_a_pty() {
        let error = PtyProcess::strings(&["valid", "bad\0value"], "argument").unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("NUL"));
    }

    /// The defect: a line typed while the pane is starting was drawn once by the host line
    /// discipline and once more by the guest, so the user saw their own input twice.
    #[test]
    fn a_line_typed_during_launch_is_drawn_once_not_twice() {
        let mut master = Master::open();

        let slave = open_slave(&master);
        master.type_line(TYPED);
        let drawn = {
            let guest = start_stand_in_guest(slave);
            master.release_launch_state();
            let drawn = master.drain(std::time::Duration::from_secs(5));
            stop(guest);
            drawn
        };

        assert_eq!(
            drawn.matches("hi").count(),
            1,
            "the launch window must not echo what the guest echoes again; pane drew {drawn:?}"
        );
    }

    /// What the fix must not take with it. The banner promises the typed line runs when the shell
    /// starts, which is `ICANON` holding it in the input queue; `InterruptMask` covers this same
    /// window on the assumption the discipline still generates the signals.
    #[test]
    fn the_launch_window_still_queues_the_line_and_still_generates_signals() {
        let mut master = Master::open();
        let slave = open_slave(&master);
        let flags = local_flags(&slave);

        assert_ne!(flags & libc::ICANON, 0, "the typed line must stay queued for the shell");
        assert_ne!(
            flags & libc::ISIG,
            0,
            "the discipline must keep generating terminal signals"
        );

        master.type_line(TYPED);
        let drawn = {
            let guest = start_stand_in_guest(slave);
            master.release_launch_state();
            let drawn = master.drain(std::time::Duration::from_secs(5));
            stop(guest);
            drawn
        };

        assert!(
            drawn.contains("hi"),
            "the queued line must still reach the shell that starts after it was typed; pane drew {drawn:?}"
        );
    }
}
