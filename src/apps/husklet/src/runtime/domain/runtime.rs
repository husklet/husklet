use std::io;

use hl_container::{Config, Containers};
use hl_images::remote::{Auth, Registry};
use hl_images::{Images, Platform, Reference, RuntimeOverrides};
use hl_ws::Arch;

use crate::config::WorkspaceConfig;
use crate::paths;

use super::{Configuration, CONTAINER, SIGNATURE};

/// Composes the container capabilities that back one workspace execution domain.
pub(super) struct Runtime;

impl Runtime {
    pub(super) async fn checkpoint(
        containers: &Containers,
        docker: Option<&crate::runtime::resources::Daemon>,
    ) -> io::Result<()> {
        let executions = containers.executions();
        executions.require_checkpointable().await.map_err(io::Error::other)?;
        if let Some(daemon) = docker {
            daemon.close(crate::runtime::resources::Close::Checkpoint)?;
        }
        let checkpoint = match executions.checkpoint_all(std::time::Duration::from_secs(30)).await {
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

    pub(super) async fn open(workspace: &WorkspaceConfig) -> io::Result<(Containers, Platform)> {
        if workspace.gui || workspace.cuda.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "GUI and CUDA workspaces are unavailable while Surface is being replaced",
            ));
        }
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
        // The engine's AArch64 persistent translation cache currently corrupts dynamic
        // interpreter/exec relocations on a warm load. Keep execution correct until the engine
        // regression is fixed; this is application-selected policy, not a container workaround.
        let containers = Containers::builder(Config::new(&root))
            .images(images)
            .checkpoints(checkpoints)
            .build()
            .await
            .map_err(io::Error::other)?;
        Ok((containers, platform))
    }

    pub(super) async fn ensure_container(containers: &Containers, workspace: &WorkspaceConfig) -> io::Result<()> {
        let signature = Configuration::new(workspace).signature()?;
        match containers.inspect(CONTAINER).await {
            Ok(container) => {
                let stored = container.spec.labels.get(SIGNATURE);
                let session = crate::runtime::session::Session::from_labels(&container.spec.labels);
                if stored == Some(&signature) && session.is_ok() {
                    if !container.state.is_active() {
                        containers.start(CONTAINER).await.map_err(io::Error::other)?;
                    }
                    return Ok(());
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
            command: Some(vec!["-c".into(), "while :; do sleep 2147483647 & wait $!; done".into()]),
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
                    container.spec.name.as_deref().unwrap_or(container.id.as_str())
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
