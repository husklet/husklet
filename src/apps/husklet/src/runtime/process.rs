//! Lifecycle control for a process owning a Unix socket.

use std::io;

pub(super) use ffi::CommandSession;

pub(super) struct Peer {
    process: libc::pid_t,
}

impl Peer {
    pub(super) fn new(connection: &std::os::unix::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            process: ffi::process(connection)?,
        })
    }

    pub(super) fn stop(
        self,
        signal: libc::c_int,
        timeout: std::time::Duration,
        reconnect: impl Fn() -> io::Result<std::os::unix::net::UnixStream>,
    ) -> io::Result<()> {
        self.signal_group(signal)?;
        self.wait(timeout, reconnect)
    }

    pub(super) fn wait(
        self,
        timeout: std::time::Duration,
        reconnect: impl Fn() -> io::Result<std::os::unix::net::UnixStream>,
    ) -> io::Result<()> {
        let process = self.process;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let connection = match reconnect() {
                Err(error) if Self::offline(&error) => return Ok(()),
                Err(error) => return Err(error),
                Ok(connection) => connection,
            };
            let peer = Self::new(&connection)?;
            if peer.process != process {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                peer.signal_group(libc::SIGKILL)?;
                return Self::wait_offline(reconnect, std::time::Duration::from_secs(2));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    pub(super) fn request(&self, signal: libc::c_int) -> io::Result<()> {
        if self.process <= 1 {
            return Err(io::Error::other("socket owner reported an invalid process identity"));
        }
        self.signal(signal)?;
        Ok(())
    }

    fn wait_offline(
        reconnect: impl Fn() -> io::Result<std::os::unix::net::UnixStream>,
        timeout: std::time::Duration,
    ) -> io::Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match reconnect() {
                Err(error) if Self::offline(&error) => return Ok(()),
                Err(error) => return Err(error),
                Ok(_) if std::time::Instant::now() >= deadline => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "socket owner did not stop"));
                }
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }

    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        ffi::signal(self.process, signal)
    }

    fn signal_group(&self, signal: libc::c_int) -> io::Result<()> {
        ffi::signal_group(self.process, signal)
    }

    pub(super) fn offline(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
        )
    }
}

/// Private Unix process-control boundary consumed through safe owned values.
mod ffi {
    // The signalling and peer-credential calls in this boundary are `unsafe` libc entry points.
    #![allow(unsafe_code)]

    use std::io;
    use std::os::fd::AsRawFd;

    pub trait CommandSession {
        fn start_session(&mut self);
    }

    impl CommandSession for std::process::Command {
        fn start_session(&mut self) {
            use std::os::unix::process::CommandExt;

            #[cfg(target_os = "linux")]
            // SAFETY: `getpid` takes no pointers, retains no Rust storage, invokes no callback,
            // and cannot unwind. Capture it before fork so the child can close the race between
            // `prctl` and an owner that exits immediately after spawning it.
            let owner = unsafe { libc::getpid() };

            // SAFETY: the hook runs in the child after `fork` and before `exec`. It invokes only
            // value-only libc syscalls, retains no Rust storage, acquires no lock, allocates
            // nothing, invokes no destructor, and cannot unwind across the ABI. Linux prctl
            // binds the private engine session to its launcher so a killed launcher cannot leave
            // a detached engine holding its bootstrap descriptors indefinitely.
            unsafe {
                self.pre_exec(move || {
                    #[cfg(target_os = "linux")]
                    {
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                            return Err(io::Error::last_os_error());
                        }
                        if libc::getppid() != owner {
                            return Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "engine launcher exited during spawn",
                            ));
                        }
                    }
                    if libc::setsid() < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
    }

    #[hl_design::classify(domain = "unix")]
    pub(super) fn signal(process: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
        signal_process_with(process, signal, |target, signal| {
            // SAFETY: the target and signal are integer process identities. `kill` retains no
            // Rust storage, invokes no callback, and cannot unwind.
            let status = unsafe { libc::kill(target, signal) };
            if status == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
    }

    pub(super) fn signal_group(process: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
        signal_group_with(process, signal, |target, signal| {
            // SAFETY: as above; a negative target addresses the owner process group.
            let status = unsafe { libc::kill(target, signal) };
            if status == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
    }

    pub(super) fn signal_process_with(
        process: libc::pid_t,
        signal: libc::c_int,
        mut deliver: impl FnMut(libc::pid_t, libc::c_int) -> io::Result<()>,
    ) -> io::Result<()> {
        validate(process)?;
        match deliver(process, signal) {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn signal_group_with(
        process: libc::pid_t,
        signal: libc::c_int,
        mut deliver: impl FnMut(libc::pid_t, libc::c_int) -> io::Result<()>,
    ) -> io::Result<()> {
        validate(process)?;
        // Domain and daemon owners call setsid, making their verified PID the process-group ID.
        // Address the group so terminating its leader cannot strand helpers. A non-group peer
        // falls back to its kernel-verified PID.
        match deliver(-process, signal) {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => match deliver(process, signal) {
                Ok(()) => Ok(()),
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    fn validate(process: libc::pid_t) -> io::Result<()> {
        if process > 1 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid socket owner identity",
            ))
        }
    }

    #[cfg(target_os = "macos")]
    #[hl_design::classify(domain = "unix")]
    pub(super) fn process(connection: &std::os::unix::net::UnixStream) -> io::Result<libc::pid_t> {
        let mut process: libc::pid_t = 0;
        let mut length = std::mem::size_of_val(&process) as libc::socklen_t;
        // SAFETY: `process` and `length` describe writable storage of the exact size expected by
        // `LOCAL_PEERPID`; `connection` owns the descriptor through the call. The kernel retains no
        // pointer, Rust has no concurrent alias, no callback is invoked, and the ABI cannot unwind.
        let status = unsafe {
            libc::getsockopt(
                connection.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                std::ptr::addr_of_mut!(process).cast(),
                &raw mut length,
            )
        };
        if status == 0 && length as usize == std::mem::size_of_val(&process) {
            Ok(process)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn process(connection: &std::os::unix::net::UnixStream) -> io::Result<libc::pid_t> {
        // SAFETY: `ucred` is a plain C value with no invalid bit patterns, and zero initialization
        // creates no references, aliases, or destructor obligations.
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: `credentials` and `length` describe writable storage of the exact size expected by
        // `SO_PEERCRED`; `connection` owns the descriptor through the call. The kernel retains no
        // pointer, Rust has no concurrent alias, no callback is invoked, and the ABI cannot unwind.
        let status = unsafe {
            libc::getsockopt(
                connection.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(credentials).cast(),
                &raw mut length,
            )
        };
        if status == 0 && length as usize == std::mem::size_of_val(&credentials) {
            Ok(credentials.pid)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ffi, Peer};
    use std::io;

    #[test]
    fn unix_peer_identity_comes_from_the_kernel() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("peer.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || listener.accept().unwrap().0);
        let client = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let accepted = server.join().unwrap();

        assert_eq!(Peer::new(&client).unwrap().process, std::process::id() as i32);
        drop(accepted);
    }

    #[test]
    fn session_owner_signal_targets_the_owned_group() {
        let mut calls = Vec::new();
        ffi::signal_group_with(42, libc::SIGTERM, |target, signal| {
            calls.push((target, signal));
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, [(-42, libc::SIGTERM)]);
    }

    #[test]
    fn control_request_targets_only_the_verified_peer() {
        let mut calls = Vec::new();
        ffi::signal_process_with(42, libc::SIGHUP, |target, signal| {
            calls.push((target, signal));
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, [(42, libc::SIGHUP)]);
    }

    #[test]
    fn non_group_peer_falls_back_to_the_verified_process() {
        let mut calls = Vec::new();
        ffi::signal_group_with(42, libc::SIGKILL, |target, signal| {
            calls.push((target, signal));
            if target < 0 {
                Err(io::Error::from_raw_os_error(libc::ESRCH))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, [(-42, libc::SIGKILL), (42, libc::SIGKILL)]);
    }
}
