//! One durable container execution domain per workspace.

use crate::config::WorkspaceConfig;
use crate::paths;
use hl_container::{Config, Containers, Devices};
use hl_images::remote::{Auth, Registry};
use hl_images::{Images, Platform, Reference, RuntimeOverrides};
use hl_ws::Arch;
use std::io;
use std::path::PathBuf;

mod close;
mod configuration;
mod identity;
mod lifecycle;
mod publication;
mod restore;

use crate::runtime::process::Peer;
use close::ResultFile;
use configuration::Configuration;
use identity::RuntimeIdentity;
use lifecycle::{Disposition, Lease, Shutdown};
use publication::{ConfigurationIdentity as PublishedConfiguration, Protocol as PublishedProtocol};
use restore::RestoreSummary;

const CONTAINER: &str = "workspace";
const SIGNATURE: &str = "husklet.workspace.signature";
const PROTOCOL: &str = "3";

/// Owns the host process and socket serving one workspace's persistent execution domain.
pub struct Domain {
    directory: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Close {
    Kill,
    Continue,
}

impl Domain {
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            directory: workspace.storage_dir(&paths::hl_root()).join("runtime"),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.directory.join("domain.sock")
    }

    fn control(&self) -> PathBuf {
        self.directory.join("control.sock")
    }

    /// Starts the workspace domain process when needed and waits until its API accepts connections.
    pub fn ensure(&self, workspace: &WorkspaceConfig) -> io::Result<PathBuf> {
        std::fs::create_dir_all(&self.directory)?;
        let _startup = Lease::acquire_wait(
            self.directory.join("startup.lock"),
            std::time::Duration::from_secs(180),
        )?;
        if let Ok(connection) = std::os::unix::net::UnixStream::connect(self.socket()) {
            if PublishedProtocol::new(&self.directory).compatible()? {
                PublishedConfiguration::new(&self.directory).validate(workspace)?;
                return Ok(self.socket());
            }
            Peer::new(connection)?.stop(
                libc::SIGTERM,
                std::time::Duration::from_secs(10),
                || std::os::unix::net::UnixStream::connect(self.socket()),
            )?;
            Lease::wait_available(
                self.directory.join("domain.lock"),
                std::time::Duration::from_secs(10),
            )?;
        }
        match std::fs::remove_file(self.socket()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let output = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join("domain.log"))?;
        let errors = output.try_clone()?;
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args(["--worker", "domain", &workspace.name])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(output))
            .stderr(std::process::Stdio::from(errors));
        if let Some(capture) = crate::runtime::gpu::CaptureOptions::configured()? {
            command.args(capture.worker_arguments()?);
        }
        // SAFETY: the hook runs after `fork` and before `exec` in the child. It only invokes
        // the async-signal-safe `setsid` syscall and converts its errno into an I/O error.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = command.spawn()?;
        self.wait_for_start(child, std::time::Duration::from_secs(180))
    }

    /// Stops this workspace execution domain according to the user's close choice.
    pub fn close(&self, choice: Close) -> io::Result<()> {
        let connection = match std::os::unix::net::UnixStream::connect(self.socket()) {
            Ok(connection) => connection,
            Err(error) if Peer::offline(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        match choice {
            Close::Kill => {
                let peer = Peer::new(connection)?;
                Shutdown::request(&self.control(), Disposition::Kill)?;
                peer.wait(std::time::Duration::from_secs(45), || {
                    std::os::unix::net::UnixStream::connect(self.socket())
                })
            }
            Close::Continue => {
                let result = ResultFile::new(&self.directory);
                result.clear()?;
                drop(connection);
                Shutdown::request(&self.control(), Disposition::Checkpoint)?;
                result.wait(&self.socket(), std::time::Duration::from_secs(90))
            }
        }
    }

    pub fn take_restore_summary(workspace: &WorkspaceConfig) -> io::Result<Option<String>> {
        RestoreSummary::new(workspace).take()
    }

    fn wait_for_start(
        &self,
        mut child: std::process::Child,
        timeout: std::time::Duration,
    ) -> io::Result<PathBuf> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(self.socket()).is_ok()
                && PublishedProtocol::new(&self.directory).compatible()?
            {
                return Ok(self.socket());
            }
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "workspace execution domain exited before publishing its API ({status}); see {}",
                    self.directory.join("domain.log").display()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "workspace execution domain did not publish {}",
                self.socket().display()
            ),
        ))
    }

    pub async fn serve(workspace: &WorkspaceConfig) -> io::Result<()> {
        let owner = Self::new(workspace);
        tokio::fs::create_dir_all(&owner.directory).await?;
        let _lease = Lease::acquire(owner.directory.join("domain.lock"))?;
        let (containers, platform) = Runtime::open(workspace).await?;
        let mut failures = Runtime::remove_stale_executions(&containers).await?;
        Runtime::ensure_container(&containers, workspace).await?;
        failures.extend(Runtime::restore_checkpoints(&containers).await?);
        RestoreSummary::new(workspace).publish(&failures)?;
        let configuration = PublishedConfiguration::new(&owner.directory);
        let protocol = PublishedProtocol::new(&owner.directory);
        let close_result = ResultFile::new(&owner.directory);
        close_result.clear()?;
        protocol.publish()?;
        configuration.publish(workspace)?;
        let server = hl_daemon::Daemon::new(containers.clone())
            .platform(platform)
            .release(hl_daemon::Release::new(env!("CARGO_PKG_VERSION")))
            .server(owner.socket());
        let shutdown = Shutdown::bind(owner.control())?;
        let cleanup = std::sync::Arc::new(std::sync::Mutex::new(None));
        let completed = std::sync::Arc::clone(&cleanup);
        let stopping = containers.clone();
        let stopped_workspace = workspace.clone();
        let served = server
            .serve_with_shutdown(async move {
                loop {
                    let disposition = shutdown.clone().wait().await;
                    let docker = stopped_workspace
                        .docker_sock
                        .then(|| crate::runtime::resources::Daemon::new(&stopped_workspace));
                    let stopped = match disposition {
                        Disposition::Kill => {
                            let docker = docker
                                .map(|daemon| daemon.close(crate::runtime::resources::Close::Kill))
                                .unwrap_or(Ok(()));
                            let workspace =
                                match crate::runtime::execution::PaneExecution::clear_all(
                                    &stopped_workspace,
                                ) {
                                    Ok(()) => {
                                        stopping.shutdown(std::time::Duration::from_secs(5)).await
                                    }
                                    Err(error) => {
                                        Err(hl_container::Error::Runtime(error.to_string()))
                                    }
                                };
                            docker.and_then(|()| workspace.map_err(io::Error::other))
                        }
                        Disposition::Checkpoint => {
                            let workspace = Runtime::checkpoint(&stopping, docker.as_ref()).await;
                            if let Err(error) = close_result.publish(&workspace) {
                                if let Ok(mut result) = completed.lock() {
                                    *result = Some(Err(error));
                                }
                                break;
                            }
                            if workspace.is_err() {
                                continue;
                            }
                            workspace
                        }
                    };
                    if let Ok(mut result) = completed.lock() {
                        *result = Some(stopped);
                    }
                    break;
                }
            })
            .await
            .map_err(io::Error::other);
        let stopped = cleanup
            .lock()
            .map_err(|_| io::Error::other("workspace cleanup result lock is poisoned"))?
            .take()
            .unwrap_or_else(|| Err(io::Error::other("workspace cleanup did not run")));
        let result = match (served, stopped) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(server), Err(cleanup)) => {
                hl_log::hl_error!(
                    hl_log::tag::CONTAINER,
                    "workspace domain server failed error={server}; cleanup failed error={cleanup}"
                );
                Err(server)
            }
        };
        let unpublished = configuration.remove().and_then(|()| protocol.remove());
        match (result, unpublished) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(runtime), Err(cleanup)) => {
                hl_log::hl_error!(
                    hl_log::tag::CONTAINER,
                    "workspace runtime failed error={runtime}; configuration cleanup failed error={cleanup}"
                );
                Err(runtime)
            }
        }
    }
}

struct Runtime;

impl Runtime {
    async fn checkpoint(
        containers: &Containers,
        docker: Option<&crate::runtime::resources::Daemon>,
    ) -> io::Result<()> {
        let executions = containers.executions();
        executions
            .require_checkpointable()
            .await
            .map_err(io::Error::other)?;
        if let Some(daemon) = docker {
            daemon.close(crate::runtime::resources::Close::Checkpoint)?;
        }
        let checkpoint = match executions
            .checkpoint_all(std::time::Duration::from_secs(30))
            .await
        {
            Ok(()) => containers
                .shutdown(std::time::Duration::from_secs(5))
                .await
                .map_err(io::Error::other),
            Err(error) => Err(io::Error::other(error)),
        };
        match checkpoint {
            Ok(()) => Ok(()),
            Err(checkpoint) => {
                if let Some(daemon) = docker {
                    if let Err(restart) = daemon.ensure() {
                        return Err(io::Error::other(format!(
                            "{checkpoint}; workspace Docker service restart failed: {restart}"
                        )));
                    }
                }
                Err(checkpoint)
            }
        }
    }

    async fn open(workspace: &WorkspaceConfig) -> io::Result<(Containers, Platform)> {
        let external = Images::open(paths::images_dir()).map_err(io::Error::other)?;
        let platform = Self::platform(workspace.arch);
        let devices = Devices::new().with(crate::runtime::devices::Workspace::new(workspace)?);
        let workspace_root = workspace.storage_dir(&paths::hl_root());
        let images = Images::workspace(
            Images::open(workspace_root.join("images")).map_err(io::Error::other)?,
            external,
        );
        let checkpoints = std::sync::Arc::new(
            crate::runtime::checkpoint::WorkspaceCheckpoints::open(&workspace_root)
                .map_err(io::Error::other)?,
        );
        let root = workspace_root.join("containers");
        // The engine's AArch64 persistent translation cache currently corrupts dynamic
        // interpreter/exec relocations on a warm load. Keep execution correct until the engine
        // regression is fixed; this is application-selected policy, not a container workaround.
        let containers = Containers::builder(Config::new(&root))
            .images(images)
            .devices(devices)
            .checkpoints(checkpoints)
            .build()
            .await
            .map_err(io::Error::other)?;
        Ok((containers, platform))
    }

    async fn ensure_container(
        containers: &Containers,
        workspace: &WorkspaceConfig,
    ) -> io::Result<()> {
        let signature = Configuration::new(workspace).signature()?;
        match containers.inspect(CONTAINER).await {
            Ok(container) => {
                let stored = container.spec.labels.get(SIGNATURE);
                let session = crate::runtime::session::Session::from_labels(&container.spec.labels);
                if stored == Some(&signature) && session.is_ok() {
                    if !container.state.is_active() {
                        containers
                            .start(CONTAINER)
                            .await
                            .map_err(io::Error::other)?;
                    }
                    return Ok(());
                }
                hl_log::hl_info!(
                    hl_log::tag::CONTAINER,
                    "recreating incompatible workspace runtime container"
                );
                containers
                    .remove_force(CONTAINER)
                    .await
                    .map_err(io::Error::other)?;
            }
            Err(hl_container::Error::NotFound(_)) => {}
            Err(error) => return Err(io::Error::other(error)),
        }

        let images = containers.images().map_err(io::Error::other)?;
        let reference: Reference = workspace.image.parse().map_err(io::Error::other)?;
        let platform = Self::platform(workspace.arch);
        let image = match images.resolve(&reference).map_err(io::Error::other)? {
            Some(image) => image,
            None => images
                .pull(&Registry::new(Auth::Anonymous), reference, &platform)
                .await
                .map_err(io::Error::other)?,
        };
        let unpacked = images.unpack(&image, &platform).map_err(io::Error::other)?;
        let session = crate::runtime::session::Session::select(&images, &unpacked)?;
        let overrides = RuntimeOverrides {
            entrypoint: Some(vec!["/bin/sh".into()]),
            command: Some(vec![
                "-c".into(),
                "while :; do sleep 2147483647 & wait $!; done".into(),
            ]),
            environment: Configuration::new(workspace).environment(),
            working_directory: Some("/root".into()),
            user: Some("0:0".into()),
        };
        containers
            .create_image(&unpacked, overrides, |spec| {
                session.label(Configuration::new(workspace).container(spec, signature))
            })
            .await
            .map_err(io::Error::other)?;
        if let Err(error) = session.provision(containers).await {
            let removed = containers.remove_force(CONTAINER).await;
            return match removed {
                Ok(_) => Err(error),
                Err(rollback) => Err(io::Error::other(format!(
                    "{error}; workspace container rollback failed: {rollback}"
                ))),
            };
        }
        containers.start(CONTAINER).await.map_err(io::Error::other)
    }

    async fn remove_stale_executions(containers: &Containers) -> io::Result<Vec<String>> {
        let executions = containers.executions();
        let stale = executions.list().await.map_err(io::Error::other)?;
        let removable = stale
            .iter()
            .filter(|execution| execution.checkpoint.is_none())
            .collect::<Vec<_>>();
        for execution in &removable {
            executions
                .remove(&execution.id)
                .await
                .map_err(io::Error::other)?;
        }
        if !removable.is_empty() {
            hl_log::hl_info!(
                hl_log::tag::CONTAINER,
                "removed {} stale workspace executions",
                removable.len()
            );
        }
        Ok(Vec::new())
    }

    async fn restore_checkpoints(containers: &Containers) -> io::Result<Vec<String>> {
        let mut failures = Vec::new();
        for container in containers.list().await.map_err(io::Error::other)? {
            if container.spec.name.as_deref() == Some(CONTAINER)
                || container.state.is_active()
                || container.checkpoint.is_none()
            {
                continue;
            }
            if let Err(error) = containers.start(container.id.as_str()).await {
                failures.push(format!(
                    "{}: {error}",
                    container
                        .spec
                        .name
                        .as_deref()
                        .unwrap_or(container.id.as_str())
                ));
            }
        }
        Ok(failures)
    }

    fn platform(arch: Arch) -> Platform {
        match arch {
            Arch::Arm64 => Platform::linux_arm64(),
            Arch::Amd64 => Platform::linux_amd64(),
        }
    }
}

#[cfg(test)]
mod tests;
