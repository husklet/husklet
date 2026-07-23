//! Per-workspace hl-daemon: each workspace gets its OWN isolated Docker-API daemon (own socket, state,
//! and volumes under `~/.hl/ws/<name>/`), so `docker` inside the workspace and its overview
//! it — see only that workspace's containers. The image cache is shared (images are immutable).
//!
//! [`Daemon::ensure`] is idempotent: it returns the socket path, starting the daemon on first use.

use crate::paths;
use std::path::PathBuf;

pub struct Daemon {
    directory: PathBuf,
}

impl Daemon {
    pub fn new(workspace: &str) -> Self {
        Self {
            directory: paths::hl_root()
                .join("ws")
                .join(hl_ws::Workspace::storage_component(workspace)),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.directory.join("docker.sock")
    }

    /// Ensure the workspace's daemon is running and return its socket path. Idempotent — a live socket is
    /// reused; otherwise the daemon is spawned detached and we wait (briefly) for it to listen.
    pub fn ensure(&self) -> std::io::Result<PathBuf> {
        let dir = &self.directory;
        std::fs::create_dir_all(dir)?;
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
                format!(
                    "hl-daemon binary not found at {} (set HL_DAEMON_BIN)",
                    bin.display()
                ),
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

    fn wait_for_start(
        &self,
        mut child: std::process::Child,
        timeout: std::time::Duration,
    ) -> std::io::Result<PathBuf> {
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
    use super::Daemon;

    #[test]
    fn startup_reports_an_exited_daemon_without_waiting_for_timeout() {
        let daemon = Daemon {
            directory: tempfile::tempdir().unwrap().path().join("daemon"),
        };
        let child = std::process::Command::new("/usr/bin/false")
            .spawn()
            .unwrap();
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
        let slash = Daemon::new("a/b");
        let question = Daemon::new("a?b");

        assert_ne!(slash.directory, question.directory);
        assert!(slash.directory.ends_with("a%2Fb"));
        assert!(question.directory.ends_with("a%3Fb"));
    }
}
