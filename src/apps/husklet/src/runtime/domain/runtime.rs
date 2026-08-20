use std::io;

use hl_container::{Config, Containers};
use hl_images::remote::{Auth, Registry};
use hl_images::{Images, Platform, Reference, RuntimeOverrides};
use hl_ws::Arch;

use crate::config::WorkspaceConfig;
use crate::paths;

use super::{Configuration, CONFIGURATION_SIGNATURE, CONTAINER, RUNTIME_SIGNATURE, SIGNATURE};

/// Composes the container capabilities that back one workspace execution domain.
pub(super) struct Runtime;

/// The window a restoring start is watched for the immediate death that means the saved memory
/// image could not be rebuilt.
///
/// A restore that cannot claim a guest address fails inside the restored init before a single guest
/// instruction runs: the observed failure killed the container 55 ms after the start returned. A
/// container still alive at the end of this window is treated as restored; if it dies later, the
/// pane path's own not-running recovery still applies. The cost is paid only by a start that
/// actually restores a checkpoint.
const RESTORE_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

trait PrimaryLifecycle {
    async fn start_primary(&self) -> Result<(), PrimaryStartError>;
    async fn discard_primary_checkpoint(&self) -> Result<(), String>;
    /// Reports why a restoring start did not produce a live container, or `None` when it did.
    ///
    /// `start_primary` returns as soon as the engine process is launched, so a restore that fails
    /// while rebuilding guest memory reports success and leaves the container `Exited`. Without
    /// this the execution domain publishes its socket in front of a dead container and every
    /// terminal that follows conflicts with it.
    async fn restored_primary_failure(&self) -> io::Result<Option<String>>;
}

enum PrimaryStartError {
    Process(String),
    Repository(io::Error),
}

impl PrimaryStartError {
    fn from_container(error: hl_container::Error) -> Self {
        match error {
            hl_container::Error::Io(error) => Self::Repository(error),
            error @ (hl_container::Error::Corrupt(_)
            | hl_container::Error::Json(_)
            | hl_container::Error::Image(_)
            | hl_container::Error::TranslationCache(_)) => Self::Repository(io::Error::other(error)),
            error => Self::Process(error.to_string()),
        }
    }
}

#[cfg(test)]
mod test;

impl PrimaryLifecycle for Containers {
    async fn start_primary(&self) -> Result<(), PrimaryStartError> {
        self.start(CONTAINER).await.map_err(PrimaryStartError::from_container)
    }

    async fn discard_primary_checkpoint(&self) -> Result<(), String> {
        self.discard_checkpoint(CONTAINER)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn restored_primary_failure(&self) -> io::Result<Option<String>> {
        // `NotRunning` answers immediately for a container that has ALREADY exited, which
        // `NextExit` does not: at an unchanged generation it waits for the exit after this one and
        // would time out over the fastest restore failures, the very ones this watches for.
        let exit = tokio::time::timeout(
            RESTORE_SETTLE,
            self.wait_for(CONTAINER, hl_container::WaitCondition::NotRunning),
        )
        .await;
        match exit {
            Err(_still_running) => Ok(None),
            Ok(Ok(Some(status))) => Ok(Some(format!("the restored container exited immediately ({status:?})"))),
            Ok(Ok(None)) => Ok(Some("the restored container disappeared".to_owned())),
            Ok(Err(error)) => Err(io::Error::other(error)),
        }
    }
}

impl Runtime {
    pub(super) async fn checkpoint(
        containers: &Containers,
        docker: Option<&crate::runtime::resources::Daemon>,
    ) -> io::Result<()> {
        let docker_preparation = if let Some(daemon) = docker {
            let preparation = daemon.prepare_checkpoint()?;
            if let Some(warning) = preparation.warning() {
                hl_log::hl_error!(hl_log::tag::CONTAINER, "{warning}");
            }
            Some(preparation)
        } else {
            None
        };
        let checkpoint = match containers.checkpoint_all(std::time::Duration::from_secs(30)).await {
            Ok(()) => containers
                .shutdown(std::time::Duration::from_secs(5))
                .await
                .map_err(io::Error::other),
            Err(error) => Err(io::Error::other(error)),
        };
        let Err(checkpoint) = checkpoint else {
            return Ok(());
        };
        drop(docker_preparation);
        let Some(daemon) = docker else {
            return Err(checkpoint);
        };
        if let Err(restart) = daemon.ensure() {
            return Err(io::Error::other(format!(
                "{checkpoint}; workspace Docker service restart failed: {restart}"
            )));
        }
        Err(checkpoint)
    }

    pub(super) async fn open(workspace: &WorkspaceConfig) -> io::Result<(Containers, Platform)> {
        let external = Images::open(paths::images_dir()).map_err(io::Error::other)?;
        let platform = Self::platform(workspace.arch);
        let workspace_root = workspace.storage_dir(&paths::hl_root());
        let images = Images::workspace(
            Images::open(workspace_root.join("images")).map_err(io::Error::other)?,
            external,
        );
        let checkpoints = std::sync::Arc::new(
            crate::runtime::checkpoint::WorkspaceCheckpoints::open(&workspace_root).map_err(io::Error::other)?,
        );
        let root = workspace_root.join("containers");
        // The persistent translation cache stays unselected here, and `Config::translation_cache`
        // is the one switch that would select it. Two reasons, in order.
        //
        // The recorded warm-load corruption had a concrete cause and it is now fixed: the AArch64
        // BUS guard baked an absolute arena pointer that nothing relocated, so a restored guard
        // branched into the writing process on its first slow path (RK_BUSRESUME, PC_VERSION 14).
        // That was one cause found by one lane on one host; it is not proof there is no second.
        //
        // Selecting the cache also pins the guest image and interpreter at fixed VAs
        // (PC_IMG_BASE / PC_INTERP_BASE), which is a large guest address-layout change. Turning it
        // on is a measurement, not a comment, and it needs macOS warm-run evidence first.
        // This is application-selected policy, not a container workaround.
        let containers = Containers::builder(Config::new(&root))
            .images(images)
            .checkpoints(checkpoints)
            .build()
            .await
            .map_err(io::Error::other)?;
        Ok((containers, platform))
    }

    pub(super) async fn ensure_container(
        containers: &Containers,
        workspace: &WorkspaceConfig,
    ) -> io::Result<Vec<String>> {
        let configuration = Configuration::new(workspace);
        let signature = configuration.signature()?;
        let configuration_signature = configuration.configuration_signature()?;
        let runtime_signature = configuration.runtime_signature();
        match containers.inspect(CONTAINER).await {
            Ok(container) => {
                let mut checkpointed = container.checkpoint.is_some();
                let stored = container.spec.labels.get(SIGNATURE);
                let stored_configuration = container.spec.labels.get(CONFIGURATION_SIGNATURE);
                let stored_runtime = container.spec.labels.get(RUNTIME_SIGNATURE);
                let session = crate::runtime::session::Session::from_labels(&container.spec.labels);
                let reusable = session.is_ok()
                    && match stored_configuration {
                        Some(value) => value == &configuration_signature,
                        None if stored == Some(&signature) => true,
                        None => configuration.legacy_container_compatible(&container.spec)?,
                    };
                if reusable {
                    let runtime_reusable = stored_runtime == Some(&runtime_signature)
                        || (stored_runtime.is_none() && stored == Some(&signature));
                    if !runtime_reusable {
                        containers
                            .discard_checkpoint(CONTAINER)
                            .await
                            .map_err(io::Error::other)?;
                        checkpointed = false;
                        let executions = containers.executions();
                        for execution in executions.list().await.map_err(io::Error::other)? {
                            executions.remove(&execution.id).await.map_err(io::Error::other)?;
                        }
                    }
                    if stored_configuration != Some(&configuration_signature) {
                        containers
                            .set_label(CONTAINER, CONFIGURATION_SIGNATURE, &configuration_signature)
                            .await
                            .map_err(io::Error::other)?;
                    }
                    if stored != Some(&signature) {
                        containers
                            .set_label(CONTAINER, SIGNATURE, &signature)
                            .await
                            .map_err(io::Error::other)?;
                    }
                    // Publish runtime compatibility last: a crash before this point retries
                    // checkpoint and execution cleanup rather than trusting a partial migration.
                    if stored_runtime != Some(&runtime_signature) {
                        containers
                            .set_label(CONTAINER, RUNTIME_SIGNATURE, &runtime_signature)
                            .await
                            .map_err(io::Error::other)?;
                    }
                }
                if reusable {
                    return if container.state.is_active() {
                        Ok(Vec::new())
                    } else {
                        Self::start_primary(containers, checkpointed).await
                    };
                }
                hl_log::hl_info!(
                    hl_log::tag::CONTAINER,
                    "recreating incompatible workspace runtime container"
                );
                containers.remove_force(CONTAINER).await.map_err(io::Error::other)?;
            }
            Err(hl_container::Error::NotFound(_)) => {}
            Err(error) => return Err(io::Error::other(error)),
        }

        let images = containers.images().map_err(io::Error::other)?;
        let reference: Reference = workspace.image.parse().map_err(io::Error::other)?;
        let platform = Self::platform(workspace.arch);
        let image = if let Some(image) = images.resolve(&reference).map_err(io::Error::other)? {
            image
        } else {
            #[cfg(test)]
            if std::env::var_os("HL_TEST_FORBID_REMOTE_IMAGES").is_some() {
                return Err(io::Error::other(format!(
                    "remote image resolution was reached for {reference}; the seeded workspace container was not reusable"
                )));
            }
            images
                .pull(&Registry::new(Auth::Anonymous), reference, &platform)
                .await
                .map_err(io::Error::other)?
        };
        let unpacked = images.unpack(&image, &platform).map_err(io::Error::other)?;
        let session = crate::runtime::session::Session::select(&images, &unpacked)?;
        let overrides = RuntimeOverrides {
            entrypoint: Some(vec!["/bin/sh".into()]),
            command: Some(vec!["-c".into(), "while :; do sleep 2147483647 & wait $!; done".into()]),
            environment: Configuration::new(workspace).environment(),
            working_directory: Some("/root".into()),
            user: Some("0:0".into()),
        };
        containers
            .create_image(&unpacked, overrides, |spec| {
                session.label(Configuration::new(workspace).container(
                    spec,
                    signature,
                    configuration_signature,
                    runtime_signature,
                ))
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
        Self::start_primary(containers, false).await
    }

    /// Starts the workspace's primary process without making one process-local launch failure fatal
    /// to the execution domain. A failed checkpoint gets one clean-start attempt after its durable
    /// marker is removed; inability to update that durable marker remains a repository-wide error.
    async fn start_primary(lifecycle: &impl PrimaryLifecycle, checkpointed: bool) -> io::Result<Vec<String>> {
        let first = match lifecycle.start_primary().await {
            Ok(()) if !checkpointed => return Ok(Vec::new()),
            Ok(()) => match lifecycle.restored_primary_failure().await? {
                None => return Ok(Vec::new()),
                Some(reason) => reason,
            },
            Err(PrimaryStartError::Process(error)) => error,
            Err(PrimaryStartError::Repository(error)) => return Err(error),
        };
        if !checkpointed {
            return Ok(vec![format!("workspace: start failed: {first}")]);
        }
        lifecycle
            .discard_primary_checkpoint()
            .await
            .map_err(|error| io::Error::other(format!("discard workspace checkpoint: {error}")))?;
        match lifecycle.start_primary().await {
            Ok(()) => Ok(vec![format!(
                "workspace: checkpoint restore failed ({first}); started a fresh primary process"
            )]),
            Err(PrimaryStartError::Process(fresh)) => Ok(vec![format!(
                "workspace: checkpoint restore failed ({first}); fresh start failed: {fresh}"
            )]),
            Err(PrimaryStartError::Repository(error)) => Err(error),
        }
    }

    pub(super) async fn remove_stale_executions(containers: &Containers) -> io::Result<Vec<String>> {
        let executions = containers.executions();
        let stale = executions.list().await.map_err(io::Error::other)?;
        let removable = stale
            .iter()
            .filter(|execution| execution.checkpoint.is_none())
            .collect::<Vec<_>>();
        for execution in &removable {
            executions.remove(&execution.id).await.map_err(io::Error::other)?;
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

    pub(super) async fn restore_checkpoints(containers: &Containers) -> io::Result<Vec<String>> {
        let targets = containers
            .list()
            .await
            .map_err(io::Error::other)?
            .into_iter()
            .filter(|container| {
                container.spec.name.as_deref() != Some(CONTAINER)
                    && !container.state.is_active()
                    && container.checkpoint.is_some()
            })
            .map(|container| {
                let label = container.spec.name.clone().unwrap_or_else(|| container.id.to_string());
                (container.id.to_string(), label)
            });
        Self::restore_independently(
            targets,
            |id| async move { containers.start(&id).await },
            || async { containers.executions().restore_checkpoints().await },
            |id| async move { containers.executions().remove(&id).await },
        )
        .await
        .map_err(io::Error::other)
    }

    /// Starts each checkpointed container, reattaches each checkpointed execution, and settles the
    /// records of the executions that could not be reattached.
    ///
    /// A refused reattach is terminal for that record: nothing later in the workspace's life can
    /// revive it, and `remove_stale_executions` only discards records that never reached a
    /// checkpoint. Left in place, a refused record is re-listed by every subsequent restore, so
    /// cycling one workspace grows the notice by one line per close -- the second reopen reports
    /// two unresumable programs, the third reports three -- while its journal and checkpoint
    /// objects are never released. Discard the record once, in the same pass that reports it.
    pub(super) async fn restore_independently<I, J, E, S, SF, X, XF, D, DF>(
        targets: I,
        mut start: S,
        restore_executions: X,
        mut discard: D,
    ) -> Result<Vec<String>, E>
    where
        I: IntoIterator<Item = (String, String)>,
        J: Clone + std::fmt::Display,
        E: std::fmt::Display,
        S: FnMut(String) -> SF,
        SF: std::future::Future<Output = Result<(), E>>,
        X: FnOnce() -> XF,
        XF: std::future::Future<Output = Result<Vec<(J, E)>, E>>,
        D: FnMut(J) -> DF,
        DF: std::future::Future<Output = Result<(), E>>,
    {
        let mut failures = Vec::new();
        for (id, label) in targets {
            if let Err(error) = start(id).await {
                failures.push(format!("{label}: {error}"));
            }
        }
        for (id, error) in restore_executions().await? {
            failures.push(format!("execution {id}: {error}"));
            // Reported, therefore settled. A discard failure is its own diagnostic and must not
            // suppress the refusal above, which is the line the reader acts on.
            if let Err(error) = discard(id.clone()).await {
                hl_log::hl_info!(
                    hl_log::tag::CONTAINER,
                    "restore could not discard the unresumable execution {id}: {error}"
                );
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
