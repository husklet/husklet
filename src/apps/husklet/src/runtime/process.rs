//! Lifecycle control for a process owning a Unix socket.

use std::io;

pub(super) use ffi::CommandSession;

#[cfg(test)]
pub(super) fn signal_for_test(process: u32, signal: libc::c_int) -> io::Result<()> {
    let process = libc::pid_t::try_from(process)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process identity exceeds pid_t"))?;
    ffi::signal(process, signal)
}

#[cfg(test)]
pub(super) fn signal_group_for_test(process: u32, signal: libc::c_int) -> io::Result<()> {
    let process = libc::pid_t::try_from(process)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process identity exceeds pid_t"))?;
    ffi::signal_group(process, signal)
}

#[cfg(test)]
pub(super) fn process_exists_for_test(process: u32) -> io::Result<bool> {
    let process = libc::pid_t::try_from(process)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process identity exceeds pid_t"))?;
    ffi::exists(process)
}

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
        &self,
        timeout: std::time::Duration,
        reconnect: impl Fn() -> io::Result<std::os::unix::net::UnixStream>,
    ) -> io::Result<()> {
        let process = self.process;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let connection = match reconnect() {
                Err(error) if Self::offline(&error) => break,
                Err(error) => return Err(error),
                Ok(connection) => connection,
            };
            let peer = Self::new(&connection)?;
            if peer.process != process {
                break;
            }
            if std::time::Instant::now() >= deadline {
                peer.signal_group(libc::SIGKILL)?;
                Self::wait_offline(reconnect, std::time::Duration::from_secs(2))?;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        match self.signal_group(libc::SIGKILL) {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            Err(error) => Err(error),
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
            // async-signal-safe libc entry points over storage this frame owns for the whole call,
            // retains no Rust storage, acquires no lock, allocates nothing, invokes no destructor,
            // and cannot unwind across the ABI. Linux prctl binds the private engine session to its
            // launcher so a killed launcher cannot leave a detached engine holding its bootstrap
            // descriptors indefinitely.
            unsafe {
                self.pre_exec(move || {
                    // Everything this boundary detaches is supervised by signal afterwards: the
                    // checkpoint request, the stop, and the process-group cleanup. A signal mask is
                    // inherited across `fork` and preserved across `exec`, and registering this hook
                    // is what takes the spawn off `posix_spawn`, whose attributes would have handed
                    // the child an empty one. Restore that guarantee rather than letting a session
                    // start with a request its supervisor can never deliver.
                    let mut deliverable: libc::sigset_t = std::mem::zeroed();
                    if libc::sigemptyset(&raw mut deliverable) < 0
                        || libc::sigprocmask(libc::SIG_SETMASK, &raw const deliverable, std::ptr::null_mut()) < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
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

    #[cfg(test)]
    pub(super) fn exists(process: libc::pid_t) -> io::Result<bool> {
        validate(process)?;
        // SAFETY: signal zero performs existence and permission checking only. The integer process
        // identity carries no Rust alias, the kernel retains nothing, and the call cannot unwind.
        if unsafe { libc::kill(process, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(error),
        }
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
    use super::{ffi, process_exists_for_test, Peer};
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
    fn liveness_distinguishes_a_reaped_process_from_an_idempotent_signal() {
        assert!(process_exists_for_test(std::process::id()).unwrap());
        let mut child = std::process::Command::new("/bin/true").spawn().unwrap();
        let identity = child.id();
        child.wait().unwrap();
        assert!(!process_exists_for_test(identity).unwrap());
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

#[cfg(test)]
mod wait_cleanup_test {
    use super::{ffi, CommandSession as _, Peer};
    use std::io::{BufRead as _, Write as _};

    const HELPER: &str = "runtime::process::wait_cleanup_test::socket_owner_helper";

    #[test]
    #[ignore = "subprocess helper"]
    fn socket_owner_helper() {
        let Ok(socket) = std::env::var("HL_TEST_OWNER_SOCKET") else {
            return;
        };
        let mut connection = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let descendant_output = connection.try_clone().unwrap();
        let descendant = std::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::from(std::os::fd::OwnedFd::from(descendant_output)))
            .spawn()
            .unwrap();
        let descendant = std::mem::ManuallyDrop::new(descendant);
        writeln!(connection, "{}", descendant.id()).unwrap();
    }

    #[test]
    fn wait_kills_descendants_after_the_socket_owner_exits() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("owner.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", HELPER, "--ignored", "--nocapture"])
            .env("HL_TEST_OWNER_SOCKET", &socket);
        command.start_session();
        let mut owner = command.spawn().unwrap();
        let (connection, _) = listener.accept().unwrap();
        let peer = Peer::new(&connection).unwrap();
        let descendant_liveness = connection.try_clone().unwrap();
        let mut descendant_line = String::new();
        std::io::BufReader::new(&connection)
            .read_line(&mut descendant_line)
            .unwrap();
        let descendant = descendant_line.trim().parse::<libc::pid_t>().unwrap();
        drop(connection);
        drop(listener);
        std::fs::remove_file(&socket).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while owner.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        peer.wait(std::time::Duration::from_secs(1), || {
            std::os::unix::net::UnixStream::connect(&socket)
        })
        .unwrap();
        owner.wait().unwrap();

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            sender.send(std::io::read_to_string(descendant_liveness)).unwrap();
        });
        let closed = receiver.recv_timeout(std::time::Duration::from_secs(2));
        if closed.is_err() {
            let _ = ffi::signal(descendant, libc::SIGKILL);
        }
        assert_eq!(
            closed.unwrap().unwrap(),
            "",
            "socket-owner descendant survived group cleanup"
        );
    }
}
