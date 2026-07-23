use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

pub(super) struct Shutdown;

pub(super) struct Peer {
    process: libc::pid_t,
    connection: std::os::unix::net::UnixStream,
}

impl Peer {
    pub(super) fn new(connection: std::os::unix::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            process: Self::process(&connection)?,
            connection,
        })
    }

    pub(super) fn stop(
        self,
        timeout: std::time::Duration,
        reconnect: impl Fn() -> io::Result<std::os::unix::net::UnixStream>,
    ) -> io::Result<()> {
        if self.process <= 1 {
            return Err(io::Error::other(
                "workspace domain reported an invalid peer process",
            ));
        }
        self.signal(libc::SIGTERM)?;
        drop(self.connection);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match reconnect() {
                Err(error) if Self::offline(&error) => return Ok(()),
                Err(error) => return Err(error),
                Ok(connection) => {
                    let peer = Self::new(connection)?;
                    if peer.process != self.process {
                        return Ok(());
                    }
                    if std::time::Instant::now() >= deadline {
                        peer.signal(libc::SIGKILL)?;
                        return Self::wait_offline(reconnect, std::time::Duration::from_secs(2));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
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
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "incompatible workspace domain did not stop",
                    ));
                }
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }

    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        // SAFETY: the kernel supplied this positive peer PID for the connected Unix socket.
        if unsafe { libc::kill(self.process, signal) } == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }

    fn offline(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound
                | io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
        )
    }

    #[cfg(target_os = "macos")]
    fn process(connection: &std::os::unix::net::UnixStream) -> io::Result<libc::pid_t> {
        let mut process: libc::pid_t = 0;
        let mut length = std::mem::size_of_val(&process) as libc::socklen_t;
        // SAFETY: `process` and `length` describe writable storage of the exact size expected by
        // `LOCAL_PEERPID`; `connection` owns a live Unix-domain socket descriptor.
        let status = unsafe {
            libc::getsockopt(
                connection.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                std::ptr::addr_of_mut!(process).cast(),
                &mut length,
            )
        };
        if status == 0 && length as usize == std::mem::size_of_val(&process) {
            Ok(process)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    fn process(connection: &std::os::unix::net::UnixStream) -> io::Result<libc::pid_t> {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: `credentials` and `length` describe writable storage of the exact size expected by
        // `SO_PEERCRED`; `connection` owns a live Unix-domain socket descriptor.
        let status = unsafe {
            libc::getsockopt(
                connection.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(credentials).cast(),
                &mut length,
            )
        };
        if status == 0 && length as usize == std::mem::size_of_val(&credentials) {
            Ok(credentials.pid)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Shutdown {
    pub(super) async fn wait() {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup());
        match (terminate, hangup) {
            (Ok(mut terminate), Ok(mut hangup)) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                    _ = hangup.recv() => {}
                }
            }
            _ => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
}

pub(super) struct Lease(std::fs::File);

impl Lease {
    pub(super) fn acquire(path: PathBuf) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: `file` owns a valid open descriptor for this call. `flock` does not retain
        // the descriptor or access Rust-managed memory.
        if unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "workspace execution domain is already starting",
            ));
        }
        Ok(Self(file))
    }

    pub(super) fn acquire_wait(path: PathBuf, timeout: std::time::Duration) -> io::Result<Self> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match Self::acquire(path.clone()) {
                Ok(lease) => return Ok(lease),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("timed out waiting for {}", path.display()),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn wait_available(path: PathBuf, timeout: std::time::Duration) -> io::Result<()> {
        drop(Self::acquire_wait(path, timeout)?);
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // SAFETY: the lease still owns its open descriptor. Unlocking neither retains the
        // descriptor nor accesses Rust-managed memory.
        let _ = unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::{Lease, Peer};

    #[test]
    fn unix_peer_identity_comes_from_the_kernel() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("peer.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || listener.accept().unwrap().0);
        let client = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let accepted = server.join().unwrap();

        assert_eq!(
            Peer::new(client).unwrap().process,
            std::process::id() as i32
        );
        drop(accepted);
    }

    #[test]
    fn lease_waits_for_the_previous_owner_to_finish_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domain.lock");
        let lease = Lease::acquire(path.clone()).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            drop(lease);
        });
        let started = std::time::Instant::now();

        Lease::wait_available(path, std::time::Duration::from_secs(1)).unwrap();

        assert!(started.elapsed() >= std::time::Duration::from_millis(40));
        release.join().unwrap();
    }
}
