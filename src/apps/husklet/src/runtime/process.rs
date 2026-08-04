//! Lifecycle control for a process owning a Unix socket.

use std::io;
use std::os::fd::AsRawFd;

pub(super) struct Peer {
    process: libc::pid_t,
}

impl Peer {
    pub(super) fn new(connection: std::os::unix::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            process: Self::process(&connection)?,
        })
    }

    pub(super) fn stop(
        self,
        signal: libc::c_int,
        timeout: std::time::Duration,
        reconnect: impl Fn() -> io::Result<std::os::unix::net::UnixStream>,
    ) -> io::Result<()> {
        self.request(signal)?;
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
            match reconnect() {
                Err(error) if Self::offline(&error) => return Ok(()),
                Err(error) => return Err(error),
                Ok(connection) => {
                    let peer = Self::new(connection)?;
                    if peer.process != process {
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

    pub(super) fn offline(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
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
        // SAFETY: `ucred` is a plain C value and zero is valid initialization before `getsockopt`.
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

#[cfg(test)]
mod tests {
    use super::Peer;

    #[test]
    fn unix_peer_identity_comes_from_the_kernel() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("peer.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || listener.accept().unwrap().0);
        let client = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let accepted = server.join().unwrap();

        assert_eq!(Peer::new(client).unwrap().process, std::process::id() as i32);
        drop(accepted);
    }
}
