//! Per-workspace Docker API process and its workspace-owned storage.
//!
//! [`Daemon::ensure`] is idempotent: it returns the socket path, starting the daemon on first use.

use crate::config::WorkspaceConfig;
use crate::ffi::ExclusiveFileLock;
use crate::paths;
use crate::runtime::process::CommandSession as _;
use std::path::{Path, PathBuf};

/// Exclusive proof that no Docker daemon can own this workspace while it is checkpointed.
#[derive(Debug)]
pub(super) struct CheckpointPreparation {
    warning: Option<String>,
    _owner: std::fs::File,
}

impl CheckpointPreparation {
    #[must_use]
    pub(super) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }
}

pub struct Daemon {
    binary: PathBuf,
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
            binary: paths::daemon_bin(),
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
        self.ensure_locked()
    }

    fn ensure_locked(&self) -> std::io::Result<PathBuf> {
        let dir = &self.directory;
        let sock = self.socket();

        if self.is_up() {
            return Ok(sock);
        }
        // Socket reachability is not ownership. Refuse to unlink or replace a path while a daemon
        // still holds the durable data-root lease.
        let owner = self.owner_guard()?;
        // Stale socket file from a dead daemon — remove so bind() succeeds.
        match std::fs::remove_file(&sock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let bin = self.binary.clone();
        if !bin.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("dockerd binary not found at {} (set HL_DOCKERD_BIN)", bin.display()),
            ));
        }

        // The child must acquire this same lease before opening repository state. The startup lock
        // still excludes every application-launched competitor across this handoff.
        drop(owner);

        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("daemon.log"))?;
        let errlog = log.try_clone()?;
        let mut cmd = super::platform::daemon(&bin);
        cmd.arg("--root")
            .arg(dir)
            .arg("--checkpoint-compatible")
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
        std::fs::create_dir_all(&self.directory)?;
        let _startup = ExclusiveFileLock::acquire(&self.directory.join("startup.lock"))?;
        let connection = match std::os::unix::net::UnixStream::connect(self.socket()) {
            Ok(connection) => connection,
            Err(error) if crate::runtime::process::Peer::offline(&error) || Self::socket_unavailable(&error) => {
                let _owner = self.absent_owner(&error)?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let failure = self.directory.join("shutdown.error");
        let success = self.directory.join("shutdown.success");
        match choice {
            Close::Kill => {
                crate::runtime::process::Peer::new(&connection)?.stop(
                    libc::SIGTERM,
                    std::time::Duration::from_secs(45),
                    || std::os::unix::net::UnixStream::connect(self.socket()),
                )?;
            }
            Close::Checkpoint => {
                let peer = crate::runtime::process::Peer::new(&connection)?;
                self.checkpoint(&connection, &failure, &success)?;
                peer.wait(std::time::Duration::from_secs(2), || {
                    std::os::unix::net::UnixStream::connect(self.socket())
                })?;
            }
        }
        let _owner = self.owner_guard()?;
        Ok(())
    }

    /// Prepares the optional Docker service for a workspace checkpoint.
    ///
    /// An unavailable service must not make the workspace itself uncheckpointable. When a live
    /// service rejects its graceful checkpoint, stop its complete process group before allowing
    /// the workspace repository to be captured; otherwise two owners could mutate the same state.
    pub(super) fn prepare_checkpoint(&self) -> std::io::Result<CheckpointPreparation> {
        std::fs::create_dir_all(&self.directory)?;
        if let Some(parent) = self.socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _startup = ExclusiveFileLock::acquire(&self.directory.join("startup.lock"))?;
        let connection = match std::os::unix::net::UnixStream::connect(self.socket()) {
            Ok(connection) => connection,
            Err(error) if crate::runtime::process::Peer::offline(&error) || Self::socket_unavailable(&error) => {
                let owner = self.absent_owner(&error)?;
                let warning = format!("workspace Docker service was unavailable during checkpoint: {error}");
                self.record_checkpoint_warning(&warning)?;
                return Ok(CheckpointPreparation {
                    warning: Some(warning),
                    _owner: owner,
                });
            }
            Err(error) => return Err(error),
        };
        let prepared = self.prepare_live_checkpoint(&connection);
        match prepared {
            Ok(prepared) => Ok(prepared),
            Err(error) => match self.ensure_locked() {
                Ok(_) => Err(error),
                Err(restart) => Err(std::io::Error::other(format!(
                    "{error}; workspace Docker service rollback failed: {restart}"
                ))),
            },
        }
    }

    fn prepare_live_checkpoint(
        &self,
        connection: &std::os::unix::net::UnixStream,
    ) -> std::io::Result<CheckpointPreparation> {
        let peer = crate::runtime::process::Peer::new(connection)?;
        let failure = self.directory.join("shutdown.error");
        let success = self.directory.join("shutdown.success");
        let warning = match self.checkpoint(connection, &failure, &success) {
            Ok(()) => {
                peer.wait(std::time::Duration::from_secs(2), || {
                    std::os::unix::net::UnixStream::connect(self.socket())
                })?;
                None
            }
            Err(error) => {
                peer.stop(libc::SIGTERM, std::time::Duration::from_secs(5), || {
                    std::os::unix::net::UnixStream::connect(self.socket())
                })?;
                Some(format!(
                    "workspace Docker service could not checkpoint and was stopped; Docker workloads were not preserved: {error}"
                ))
            }
        };
        let owner = self.owner_guard()?;
        if let Some(warning) = warning.as_deref() {
            self.record_checkpoint_warning(warning)?;
        }
        Ok(CheckpointPreparation { warning, _owner: owner })
    }

    /// Returns a durable Docker checkpoint warning for the next workspace launch.
    pub fn checkpoint_warning(&self) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(self.directory.join("checkpoint.warning")) {
            Ok(warning) => Ok(Some(warning)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Removes a warning only after it has been included in the launch summary.
    pub fn clear_checkpoint_warning(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.directory.join("checkpoint.warning")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn record_checkpoint_warning(&self, warning: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        hl_fs::File::from(self.directory.join("checkpoint.warning")).replace(warning)
    }

    fn socket_unavailable(error: &std::io::Error) -> bool {
        error.kind() == std::io::ErrorKind::InvalidInput || error.raw_os_error() == Some(libc::ENAMETOOLONG)
    }

    fn owner_guard(&self) -> std::io::Result<std::fs::File> {
        let owner = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.directory.join("daemon.owner.lock"))?;
        fs2::FileExt::try_lock_exclusive(&owner)?;
        Ok(owner)
    }

    fn absent_owner(&self, socket: &std::io::Error) -> std::io::Result<std::fs::File> {
        self.owner_guard().map_err(|owner| {
            std::io::Error::new(
                owner.kind(),
                format!("Docker API is unavailable ({socket}) but its durable owner is still live: {owner}"),
            )
        })
    }

    /// Signals a checkpoint and accepts it only after the stopped owner publishes its commit acknowledgement.
    fn checkpoint(
        &self,
        connection: &std::os::unix::net::UnixStream,
        failure: &Path,
        success: &Path,
    ) -> std::io::Result<()> {
        for outcome in [failure, success] {
            match std::fs::remove_file(outcome) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
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
                Err(error) if crate::runtime::process::Peer::offline(&error) => break,
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
        match std::fs::read_to_string(failure) {
            Ok(message) => return Err(std::io::Error::other(message)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match std::fs::read_to_string(success) {
            Ok(acknowledgement) if acknowledgement == "ok\n" => Ok(()),
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Docker service published an invalid checkpoint acknowledgement",
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(std::io::Error::other(
                "Docker service exited without committing a checkpoint acknowledgement",
            )),
            Err(error) => Err(error),
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

#[cfg(test)]
mod tests {
    use super::Daemon;
    use crate::runtime::process::CommandSession as _;
    use fs2::FileExt as _;

    const CHECKPOINT_FAILURE_HELPER: &str = "runtime::resources::tests::daemon_checkpoint_failure_helper";

    #[test]
    #[ignore = "subprocess helper"]
    fn daemon_checkpoint_failure_helper() {
        let socket = std::env::var_os("HL_TEST_DAEMON_SOCKET").expect("daemon socket");
        let owner = std::env::var_os("HL_TEST_DAEMON_OWNER").expect("daemon owner lease");
        let ready = std::env::var_os("HL_TEST_DAEMON_READY").expect("daemon readiness");
        let owner = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(owner)
            .unwrap();
        owner.lock_exclusive().unwrap();
        let _listener = std::env::var_os("HL_TEST_DAEMON_NO_SOCKET")
            .is_none()
            .then(|| std::os::unix::net::UnixListener::bind(socket).unwrap());
        if let Some(descendant) = std::env::var_os("HL_TEST_DAEMON_DESCENDANT") {
            let child = std::process::Command::new("/bin/sh")
                .args(["-c", "trap '' HUP TERM; while :; do sleep 1; done"])
                .spawn()
                .unwrap();
            std::fs::write(descendant, child.id().to_string()).unwrap();
            std::mem::forget(child);
        }
        if let Some(success) = std::env::var_os("HL_TEST_DAEMON_SUCCESS") {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();
                std::fs::write(&ready, b"ready").unwrap();
                hangup.recv().await.expect("checkpoint request");
                std::fs::write(success, b"ok\n").unwrap();
            });
            return;
        }
        std::fs::write(ready, b"ready").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn spawn_daemon_helper(
        daemon: &Daemon,
        temporary: &tempfile::TempDir,
        socket: bool,
        descendant: bool,
        ignore_term: bool,
        acknowledge: bool,
    ) -> (std::process::Child, Option<u32>) {
        std::fs::create_dir_all(daemon.socket.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&daemon.directory).unwrap();
        let ready = temporary.path().join("ready");
        let descendant_path = temporary.path().join("descendant");
        let mut command = if ignore_term {
            let mut command = std::process::Command::new("/bin/sh");
            command.args([
                "-c",
                "trap '' TERM; exec \"$0\" --exact \"$1\" --ignored --nocapture",
                std::env::current_exe().unwrap().to_str().unwrap(),
                CHECKPOINT_FAILURE_HELPER,
            ]);
            command
        } else {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", CHECKPOINT_FAILURE_HELPER, "--ignored", "--nocapture"]);
            command
        };
        command
            .env("HL_TEST_DAEMON_SOCKET", &daemon.socket)
            .env("HL_TEST_DAEMON_OWNER", daemon.directory.join("daemon.owner.lock"))
            .env("HL_TEST_DAEMON_READY", &ready);
        if !socket {
            command.env("HL_TEST_DAEMON_NO_SOCKET", "1");
        }
        if descendant {
            command.env("HL_TEST_DAEMON_DESCENDANT", &descendant_path);
        }
        if acknowledge {
            command.env("HL_TEST_DAEMON_SUCCESS", daemon.directory.join("shutdown.success"));
        }
        command.start_session();
        let child = command.spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready.exists(), "daemon helper did not publish readiness");
        let descendant = descendant.then(|| {
            std::fs::read_to_string(descendant_path)
                .unwrap()
                .parse::<u32>()
                .unwrap()
        });
        (child, descendant)
    }

    fn assert_process_reaped(process: u32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if !std::process::Command::new("/bin/kill")
                .args(["-0", &process.to_string()])
                .status()
                .is_ok_and(|status| status.success())
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("daemon descendant {process} survived process-group cleanup");
    }

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

    #[test]
    fn unavailable_daemon_does_not_reject_workspace_checkpoint_and_remains_visible() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("x".repeat(192)));
        let daemon = Daemon::new(&workspace);

        let preparation = daemon.prepare_checkpoint().unwrap();
        let warning = preparation.warning().unwrap().to_owned();

        assert!(warning.contains("unavailable"));
        assert_eq!(daemon.checkpoint_warning().unwrap().as_deref(), Some(warning.as_str()));
        drop(preparation);
        daemon.clear_checkpoint_warning().unwrap();
        assert_eq!(daemon.checkpoint_warning().unwrap(), None);
    }

    #[test]
    fn failed_live_daemon_checkpoint_reaps_the_owner_before_workspace_capture() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("workspace"));
        let daemon = Daemon::new(&workspace);
        // A directory at the daemon's error-publication path makes graceful checkpoint setup fail
        // immediately, before any signal is sent, while leaving the live owner available for the
        // verified forced-cleanup path.
        std::fs::create_dir_all(daemon.directory.join("shutdown.error")).unwrap();
        let (mut child, descendant) = spawn_daemon_helper(&daemon, &temporary, true, true, false, false);

        let preparation = daemon.prepare_checkpoint().unwrap();
        let warning = preparation.warning().unwrap().to_owned();
        let status = child.wait().unwrap();

        assert!(!status.success());
        assert!(warning.contains("could not checkpoint and was stopped"));
        assert!(std::os::unix::net::UnixStream::connect(&daemon.socket).is_err());
        assert_eq!(daemon.checkpoint_warning().unwrap().as_deref(), Some(warning.as_str()));
        assert_process_reaped(descendant.unwrap());
    }

    #[test]
    fn daemon_crash_before_acknowledgement_is_degraded_and_reaps_the_process_group() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("workspace"));
        let daemon = Daemon::new(&workspace);
        std::fs::create_dir_all(&daemon.directory).unwrap();
        std::fs::write(daemon.directory.join("shutdown.success"), b"ok\n").unwrap();
        let (mut child, descendant) = spawn_daemon_helper(&daemon, &temporary, true, true, false, false);

        let preparation = daemon.prepare_checkpoint().unwrap();
        let status = child.wait().unwrap();

        assert!(!status.success());
        assert!(preparation
            .warning()
            .is_some_and(|warning| warning.contains("without committing a checkpoint acknowledgement")));
        assert_process_reaped(descendant.unwrap());
    }

    #[test]
    fn acknowledged_daemon_checkpoint_is_successful_and_reaps_the_process_group() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("workspace"));
        let daemon = Daemon::new(&workspace);
        let (mut child, descendant) = spawn_daemon_helper(&daemon, &temporary, true, true, false, true);

        let preparation = daemon.prepare_checkpoint().unwrap();
        let status = child.wait().unwrap();

        assert!(status.success());
        assert_eq!(preparation.warning(), None);
        assert_process_reaped(descendant.unwrap());
    }

    #[test]
    fn an_unreachable_socket_cannot_hide_a_live_durable_owner() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("workspace"));
        let daemon = Daemon::new(&workspace);
        let (mut child, _) = spawn_daemon_helper(&daemon, &temporary, false, false, false, false);

        let error = daemon.prepare_checkpoint().unwrap_err();

        assert!(error.to_string().contains("durable owner is still live"));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn warning_publication_failure_rolls_the_mutated_daemon_back() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("workspace"));
        let mut daemon = Daemon::new(&workspace);
        daemon.binary = "/usr/bin/false".into();
        std::fs::create_dir_all(daemon.directory.join("shutdown.error")).unwrap();
        std::fs::create_dir_all(daemon.directory.join("checkpoint.warning")).unwrap();
        let (mut child, _) = spawn_daemon_helper(&daemon, &temporary, true, false, false, false);

        let error = daemon.prepare_checkpoint().unwrap_err();
        let status = child.wait().unwrap();

        assert!(!status.success());
        assert!(error.to_string().contains("Is a directory"));
        assert!(
            error.to_string().contains("rollback failed"),
            "rollback was not attempted after the warning write failure: {error}"
        );
    }

    #[test]
    fn stop_wait_failure_attempts_rollback_before_returning() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        workspace.storage = Some(temporary.path().join("workspace"));
        let mut daemon = Daemon::new(&workspace);
        daemon.binary = "/usr/bin/false".into();
        std::fs::create_dir_all(daemon.directory.join("shutdown.error")).unwrap();
        let (mut child, _) = spawn_daemon_helper(&daemon, &temporary, true, false, true, false);
        let socket_parent = daemon.socket.parent().unwrap().to_owned();
        let sabotage = socket_parent.clone();
        let sabotaging = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::set_permissions(sabotage, std::fs::Permissions::from_mode(0o000)).unwrap();
        });

        let error = daemon.prepare_checkpoint().unwrap_err();
        sabotaging.join().unwrap();
        std::fs::set_permissions(socket_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();

        assert!(
            error.to_string().contains("rollback failed"),
            "rollback was not attempted after stop/wait failure: {error}"
        );
    }
}
