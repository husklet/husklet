use super::cache::{cache_volume_name, Caches};
use super::context::Context;
use super::copy::CopyContext;
use super::remote::RemoteSources;
use super::{Build, BuildError, BuildNetwork, Builder};
use hl_container::{ContainerSpec, ExitStatus, Guest, Mount, Process, VolumeSpec};
use hl_images::build::{CacheSharing, RunMount, Step};
use hl_images::snapshot::Ownerships;
use hl_images::RuntimeOverrides;
use std::path::Path;

impl Builder {
    pub(super) async fn apply_steps(
        &self,
        context: &Context<'_>,
        built: &[Build],
        root: &Path,
        steps: Vec<(Step, hl_images::RuntimeConfig)>,
        ownerships: &mut Ownerships,
        remotes: &RemoteSources,
    ) -> Result<(), BuildError> {
        for (step, inherited) in steps {
            match &step {
                Step::Copy { .. } => {
                    self.copy_step(
                        CopyContext {
                            context,
                            built,
                            root,
                            inherited: &inherited,
                            remotes,
                        },
                        &step,
                        ownerships,
                    )?;
                }
                Step::Run {
                    command,
                    environment,
                    directory,
                    shell,
                    user,
                    mounts,
                } => {
                    let directory = directory.as_deref().unwrap_or(&inherited.working_directory);
                    let runtime = inherited.merge(RuntimeOverrides {
                        environment: environment.clone(),
                        working_directory: Some(directory.into()),
                        ..RuntimeOverrides::default()
                    })?;
                    std::fs::create_dir_all(root.join(directory.trim_start_matches('/')))?;
                    let program = shell.first().ok_or_else(|| {
                        hl_images::Error::MalformedOci("RUN shell is empty".into())
                    })?;
                    let mut arguments = shell.iter().skip(1).cloned().collect::<Vec<_>>();
                    arguments.push(command.clone());
                    let mut process = Process::new(program).args(arguments).working_dir(directory);
                    for (name, value) in runtime.environment {
                        process = process.env(name, value);
                    }
                    let user = user.as_deref().unwrap_or(&inherited.user);
                    if !user.is_empty() {
                        let (uid, gid) = Process::resolve_user(user, root)?;
                        process = process.user(uid, gid);
                    }
                    let (mounts, _locks) = self.run_mounts(context, built, mounts).await?;
                    let spec = mounts.into_iter().fold(
                        ContainerSpec::from_directory(root, process).guest(self.guest()?),
                        ContainerSpec::mount,
                    );
                    let spec = self.network.container(spec);
                    let container = self.containers.create(spec).await?;
                    let id = container.id.to_string();
                    let result = async {
                        if let BuildNetwork::Named(network) = &self.network {
                            self.containers
                                .networks()
                                .connect(network, &id, hl_container::EndpointSpec::default())
                                .await?;
                        }
                        self.containers.start(&id).await?;
                        let status = self.containers.wait(&id).await?;
                        if status == ExitStatus::Code(0) {
                            Ok(())
                        } else {
                            Err(BuildError::Run(status))
                        }
                    }
                    .await;
                    let cleanup = self.containers.remove_force(&id).await;
                    match (result, cleanup) {
                        (Ok(()), Ok(_)) => {}
                        (Err(error), _) => return Err(error),
                        (Ok(()), Err(error)) => return Err(error.into()),
                    }
                }
            }
        }
        Ok(())
    }

    async fn run_mounts(
        &self,
        context: &Context<'_>,
        built: &[Build],
        requested: &[RunMount],
    ) -> Result<(Vec<Mount>, Vec<tokio::sync::OwnedMutexGuard<()>>), BuildError> {
        let mut mounts = Vec::new();
        let mut locks = Vec::new();
        for request in requested {
            match request {
                RunMount::Bind {
                    from,
                    source,
                    target,
                } => {
                    let source_root = from.map_or(context.root(), |index| built[index].root.path());
                    mounts.push(Mount::read_only(
                        Context::new(source_root).source(source)?,
                        target,
                    ));
                }
                RunMount::Cache {
                    id,
                    target,
                    sharing,
                } => {
                    let scope = id.as_deref().unwrap_or(target);
                    let name = cache_volume_name(scope, target, *sharing);
                    if matches!(sharing, CacheSharing::Locked | CacheSharing::Private) {
                        locks.push(Caches::lock(&name).await);
                    }
                    self.containers
                        .volumes()
                        .create(VolumeSpec::new(&name))
                        .await?;
                    mounts.push(Mount::volume_read_write(name, target));
                }
            }
        }
        Ok((mounts, locks))
    }

    fn guest(&self) -> Result<Guest, BuildError> {
        match (
            self.platform.os.as_str(),
            self.platform.architecture.as_str(),
        ) {
            ("linux", "arm64") => Ok(Guest::Aarch64),
            ("linux", "amd64") => Ok(Guest::X86_64),
            _ => Err(hl_images::Error::UnsupportedPlatform {
                os: self.platform.os.clone(),
                architecture: self.platform.architecture.clone(),
                variant: self.platform.variant.clone().unwrap_or_default(),
            }
            .into()),
        }
    }
}
