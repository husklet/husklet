//! Per-workspace Docker API process and its workspace-owned storage.
//!
//! [`Daemon::ensure`] is idempotent: it returns the socket path, starting the daemon on first use.

use crate::config::WorkspaceConfig;
use crate::paths;
use std::path::PathBuf;

struct Startup(std::fs::File);

impl Startup {
    fn acquire(path: &std::path::Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: `file` owns a valid descriptor. `flock` retains no pointer or Rust-managed state.
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(file))
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        // SAFETY: the lease owns this descriptor until after the unlock call.
        let _ = unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN) };
    }
}

pub struct Daemon {
    directory: PathBuf,
    socket: PathBuf,
    images: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Close {
    Kill,
    Checkpoint,
}

impl Daemon {
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        let root = workspace.storage_dir(&paths::hl_root());
        Self {
            directory: root.join("docker"),
            socket: root.join("runtime/docker.sock"),
            images: root.join("images"),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.socket.clone()
    }

    /// Ensure the workspace's daemon is running and return its socket path. Idempotent — a live socket is
    /// reused; otherwise the daemon is spawned detached and we wait (briefly) for it to listen.
    pub fn ensure(&self) -> std::io::Result<PathBuf> {
        let dir = &self.directory;
        std::fs::create_dir_all(dir)?;
        if let Some(parent) = self.socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _startup = Startup::acquire(&dir.join("startup.lock"))?;
        let sock = self.socket();

        if self.is_up() {
            return Ok(sock);
        }
        // Stale socket file from a dead daemon — remove so bind() succeeds.
        match std::fs::remove_file(&sock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let bin = daemon_bin();
        if !bin.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("hl-daemon binary not found at {} (set HL_DAEMON_BIN)", bin.display()),
            ));
        }

        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("daemon.log"))?;
        let errlog = log.try_clone()?;
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("--root")
            .arg(dir)
            .arg("--images")
            .arg(&self.images)
            .arg("--external-images")
            .arg(paths::images_dir())
            .arg("--socket")
            .arg(&sock)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log))
            .stderr(std::process::Stdio::from(errlog));
        // Detach into its own session so it outlives the launching command.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = cmd.spawn()?;
        self.wait_for_start(child, std::time::Duration::from_secs(15))
    }

    pub fn close(&self, choice: Close) -> std::io::Result<()> {
        let connection = match std::os::unix::net::UnixStream::connect(self.socket()) {
            Ok(connection) => connection,
            Err(error) if crate::runtime::process::Peer::offline(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        let failure = self.directory.join("shutdown.error");
        match choice {
            Close::Kill => crate::runtime::process::Peer::new(connection)?.stop(
                libc::SIGTERM,
                std::time::Duration::from_secs(45),
                || std::os::unix::net::UnixStream::connect(self.socket()),
            ),
            Close::Checkpoint => {
                match std::fs::remove_file(&failure) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                crate::runtime::process::Peer::new(connection)?.request(libc::SIGHUP)?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
                loop {
                    match std::fs::read_to_string(&failure) {
                        Ok(message) => return Err(std::io::Error::other(message)),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                    match std::os::unix::net::UnixStream::connect(self.socket()) {
                        Err(error) if crate::runtime::process::Peer::offline(&error) => return Ok(()),
                        Err(error) => return Err(error),
                        Ok(_) => {}
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "Docker service did not finish checkpointing",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
    }

    fn wait_for_start(&self, mut child: std::process::Child, timeout: std::time::Duration) -> std::io::Result<PathBuf> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.is_up() {
                return Ok(self.socket());
            }
            if let Some(status) = child.try_wait()? {
                return Err(std::io::Error::other(format!(
                    "hl-daemon exited before publishing its API ({status}); see {}",
                    self.directory.join("daemon.log").display()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("hl-daemon did not publish {}", self.socket().display()),
        ))
    }

    fn is_up(&self) -> bool {
        let socket = self.socket();
        socket.exists() && std::os::unix::net::UnixStream::connect(socket).is_ok()
    }
}

/// Resolve the hl-daemon binary: `HL_DAEMON_BIN`, else the installed bundle path.
fn daemon_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("HL_DAEMON_BIN") {
        return PathBuf::from(p);
    }
    paths::daemon_bin()
}

#[cfg(test)]
mod tests {
    use super::{Daemon, Startup};

    #[test]
    fn startup_reports_an_exited_daemon_without_waiting_for_timeout() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("workspace"));
        let daemon = Daemon::new(&workspace);
        let child = std::process::Command::new("/usr/bin/false").spawn().unwrap();
        let started = std::time::Instant::now();

        let error = daemon
            .wait_for_start(child, std::time::Duration::from_secs(10))
            .unwrap_err();

        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.to_string().contains("exited before publishing"));
        assert!(error.to_string().contains("daemon.log"));
    }

    #[test]
    fn distinct_workspace_names_never_share_daemon_state() {
        let slash = Daemon::new(&crate::config::WorkspaceConfig::new(
            "a/b",
            "ubuntu",
            hl_ws::Arch::Arm64,
        ));
        let question = Daemon::new(&crate::config::WorkspaceConfig::new(
            "a?b",
            "ubuntu",
            hl_ws::Arch::Arm64,
        ));

        assert_ne!(slash.directory, question.directory);
        assert!(slash.directory.ends_with("a%2Fb/docker"));
        assert!(question.directory.ends_with("a%3Fb/docker"));
        assert!(slash.images.ends_with("a%2Fb/images"));
        assert!(slash.socket.ends_with("a%2Fb/runtime/docker.sock"));
    }

    #[test]
    fn daemon_startup_is_serialized_across_callers() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("startup.lock");
        let lease = Startup::acquire(&path).unwrap();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let _next = Startup::acquire(&path).unwrap();
            started.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        drop(lease);

        assert!(waiter.join().unwrap() >= std::time::Duration::from_millis(40));
    }
}
