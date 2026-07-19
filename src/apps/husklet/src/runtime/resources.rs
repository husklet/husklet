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
        let component: String = workspace
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect();

        Self {
            directory: paths::hl_root().join("ws").join(component),
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
        let _ = std::fs::remove_file(&sock);

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
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()?;
        // Do NOT block the caller waiting for the socket — the daemon takes a few seconds to init its JIT
        // engines + discover images, and blocking here stalls every shell launch. Return the path now; the
        // docker-socket bind + the overview connect it once it is ready.
        Ok(sock)
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
