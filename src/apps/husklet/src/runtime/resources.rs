//! Per-workspace Docker API process and its workspace-owned storage.
//!
//! [`Daemon::ensure`] is idempotent: it returns the socket path, starting the daemon on first use.

use crate::config::WorkspaceConfig;
use crate::ffi::ExclusiveFileLock;
use crate::paths;
use crate::runtime::process::CommandSession as _;
use std::path::{Path, PathBuf};

pub struct Daemon {
    directory: PathBuf,
    socket: PathBuf,
    images: PathBuf,
    platform: hl_images::Platform,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Close {
    Kill,
    Checkpoint,
}

impl Daemon {
    #[must_use]
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        let root = workspace.storage_dir(&paths::hl_root());
        Self {
            directory: root.join("docker"),
            socket: root.join("runtime/docker.sock"),
            images: root.join("images"),
            platform: match workspace.arch {
                hl_ws::Arch::Arm64 => hl_images::Platform::linux_arm64(),
                hl_ws::Arch::Amd64 => hl_images::Platform::linux_amd64(),
            },
        }
    }

    #[must_use]
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
        let _startup = ExclusiveFileLock::acquire(&dir.join("startup.lock"))?;
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
                format!("dockerd binary not found at {} (set HL_DOCKERD_BIN)", bin.display()),
            ));
        }

        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("daemon.log"))?;
        let errlog = log.try_clone()?;
        let mut cmd = super::platform::daemon(&bin);
        cmd.arg("--root")
            .arg(dir)
            .arg("--images")
            .arg(&self.images)
            .arg("--external-images")
            .arg(paths::images_dir())
            .arg("--socket")
            .arg(&sock)
            .arg("--platform")
            .arg(self.platform.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log))
            .stderr(std::process::Stdio::from(errlog));
        // Detach into its own session so it outlives the launching command.
        cmd.start_session();
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
            Close::Kill => crate::runtime::process::Peer::new(&connection)?.stop(
                libc::SIGTERM,
                std::time::Duration::from_secs(45),
                || std::os::unix::net::UnixStream::connect(self.socket()),
            ),
            Close::Checkpoint => self.checkpoint(&connection, &failure),
        }
    }

    /// Signals the checkpoint and waits for the service to go offline without reporting a failure.
    fn checkpoint(&self, connection: &std::os::unix::net::UnixStream, failure: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(failure) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        crate::runtime::process::Peer::new(connection)?.request(libc::SIGHUP)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        loop {
            match std::fs::read_to_string(failure) {
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

    fn wait_for_start(&self, mut child: std::process::Child, timeout: std::time::Duration) -> std::io::Result<PathBuf> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.is_up() {
                return Ok(self.socket());
            }
            if let Some(status) = child.try_wait()? {
                return Err(std::io::Error::other(format!(
                    "dockerd exited before publishing its API ({status}); see {}",
                    self.directory.join("daemon.log").display()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("dockerd did not publish {}", self.socket().display()),
        ))
    }

    fn is_up(&self) -> bool {
        let socket = self.socket();
        socket.exists() && std::os::unix::net::UnixStream::connect(socket).is_ok()
    }
}

/// Resolve the dockerd binary: `HL_DOCKERD_BIN`, else the installed bundle path.
fn daemon_bin() -> PathBuf {
    paths::daemon_bin()
}

#[cfg(test)]
mod tests {
    use super::Daemon;

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
    fn workspace_architecture_selects_the_daemon_platform() {
        let arm = Daemon::new(&crate::config::WorkspaceConfig::new(
            "arm",
            "ubuntu",
            hl_ws::Arch::Arm64,
        ));
        let amd = Daemon::new(&crate::config::WorkspaceConfig::new(
            "amd",
            "ubuntu",
            hl_ws::Arch::Amd64,
        ));

        assert_eq!(arm.platform, hl_images::Platform::linux_arm64());
        assert_eq!(amd.platform, hl_images::Platform::linux_amd64());
    }
}
