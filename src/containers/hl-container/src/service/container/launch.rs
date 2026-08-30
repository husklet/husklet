use super::{
    Arc, Container, ContainerState, Error, JournalId, NetworkConfig, Notify, ProcessConfig, Result, Run, Running,
    Service, now_ms,
};
use crate::service::{CheckpointConfig, MemberTerminal};

impl Service {
    pub(super) async fn process_domain(&self, container: &crate::ContainerId) -> Result<hl_engine::Domain> {
        self.live
            .lock()
            .await
            .get(container)
            .map(|run| run.process.domain())
            .ok_or_else(|| crate::Error::Corrupt(format!("running container {container} has no live process domain")))
    }

    /// Prepares the terminal each sealed member of this container's restore will reattach to.
    ///
    /// The producer has to run here, before the launch, and that is not a convenience: a restoring member
    /// asks for its terminal from inside its own descriptor restore, which happens while `Runtime::start`
    /// is still executing. There is no later moment -- a pane attaches minutes afterwards, and by then the
    /// member has already bound whatever descriptors it could find.
    ///
    /// One is prepared per sealed record that names a guest pid AND was a terminal-backed session, which
    /// is every pane. A record missing either is left alone: it keeps the honest refusal it already gets
    /// rather than being seated on a terminal nothing captured.
    ///
    /// The `Io` created here is the one [`Self::reattach_exec`](crate::service::Service::reattach_exec)
    /// will hand a pane, so it is registered under the exec's journal exactly as a started session's is.
    async fn prepare_member_terminals(&self, container: &Container) -> Result<Vec<MemberTerminal>> {
        let mut prepared = Vec::new();
        for exec in self.execs.list().await? {
            if exec.container != container.id
                || exec.checkpoint.is_none()
                || !matches!(exec.state, crate::ExecState::Created)
            {
                continue;
            }
            let (Some(guest_pid), Some(size)) = (exec.guest_pid, exec.spec.process.console.terminal) else {
                continue;
            };
            let journal = JournalId::exec(exec.id.clone());
            let live_at = self.logs.cursor(&journal).await?;
            let io = self.new_exec_io(&exec, live_at).await?;
            prepared.push(MemberTerminal {
                guest_pid,
                size,
                input: io.take_input().await?,
            });
        }
        Ok(prepared)
    }

    pub(crate) async fn start(self: &Arc<Self>, reference: &str) -> Result<()> {
        let _guard = self.operations.lock().await;
        self.start_locked(reference).await
    }

    pub(super) async fn start_locked(self: &Arc<Self>, reference: &str) -> Result<()> {
        let container = self.resolve(reference).await?;
        if container.state.is_active() {
            return Err(Error::AlreadyRunning(container.id));
        }
        self.launch_locked(container, true).await
    }

    pub(super) async fn launch_locked(self: &Arc<Self>, mut container: Container, explicit: bool) -> Result<()> {
        self.launch_cleanups.lock().await.retain(|_, task| !task.is_finished());
        if let Some(error) = self.launch_cleanup_failures.lock().await.get(&container.id) {
            return Err(Error::Runtime(format!(
                "container {} launch cleanup is poisoned: {error}",
                container.id
            )));
        }
        if self.launch_cleanups.lock().await.contains_key(&container.id) {
            return Err(Error::Runtime(format!(
                "container {} is quarantined while its unpublished process is being reaped",
                container.id
            )));
        }
        self.ensure_ports_available(&container).await?;
        self.networks.attach_default_for_publication_locked(&container).await?;
        let networks = self.launch_networks(&container).await?;
        let generation = container
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::Runtime("container generation space is exhausted".into()))?;
        let journal = JournalId::container(container.id.clone());
        let start_cursor = self.logs.cursor(&journal).await?;
        let io = self.io_for_generation(&container, generation, start_cursor).await;
        let input = io.take_input().await?;
        let process_spec = container.spec.process.clone();
        let requested_mounts = container.spec.mounts.clone();
        let (rootfs, overlay, owners) = self.rootfs_launch(&container.spec.rootfs).await?;
        let mut mounts = self.volumes.resolve(&requested_mounts).await?;
        mounts.extend(self.identity.prepare(&container, &networks)?);
        let filesystem_generation = self.identity.generation(&container)?.path().to_owned();
        let checkpoint_namespace = container
            .checkpoint
            .as_ref()
            .map_or_else(|| container.id.to_string(), |checkpoint| checkpoint.namespace.clone());
        // Every sealed member this restore is about to revive needs its terminal to exist BEFORE the
        // guest starts: the member asks for it from inside its own descriptor restore. Prepared here,
        // with the launch, because this is the last moment at which it is still possible.
        let member_terminals = if container.checkpoint.is_some() {
            self.prepare_member_terminals(&container).await?
        } else {
            Vec::new()
        };
        let checkpoint = Some(crate::service::CheckpointRole::Coordinator(CheckpointConfig {
            image: self
                .checkpoints
                .open(&checkpoint_namespace)
                .map_err(|error| Error::Runtime(error.to_string()))?,
            restore: container.checkpoint.is_some(),
        }));
        let process = self
            .runtime
            .start(ProcessConfig {
                network_namespace: container.id.namespace(),
                rootfs,
                overlay,
                owners,
                filesystem_generation,
                translation_cache: self.translation_cache.clone(),
                translation_cache_observability: self.translation_cache_observability,
                translation_symbols: self.translation_symbols.clone(),
                checkpoint,
                guest: container.spec.guest,
                execution: container.spec.execution,
                process: process_spec,
                hostname: Some(container.hostname()),
                mounts,
                resources: container.spec.resources.clone(),
                isolation: container.spec.isolation,
                network_mode: container.spec.network_mode,
                networks,
                publish: container.spec.publish.clone(),
                input,
                terminal: container.spec.process.console.terminal,
                member_terminals,
                domain: None,
                domain_owner: true,
            })
            .await;
        let process = match process {
            Ok(process) => process,
            Err(error) => {
                self.retire_io_generation(&journal, &io).await;
                return Err(error);
            }
        };
        container.generation = generation;
        container.checkpoint = None;
        container.runtime_diagnostic = None;
        container.restart.started(explicit);
        if !explicit {
            container.restart.automatic();
        }
        container.state = ContainerState::Running {
            process_id: process.id(),
            started_at_ms: now_ms(),
        };
        container.health = container.spec.healthcheck.as_ref().map(|_| crate::Health::starting());
        if let Err(error) = self.containers.replace(&container).await {
            return Err(self
                .rollback_unpublished_launch(container.id.clone(), process, &journal, &io, error)
                .await);
        }
        self.emit(crate::LifecycleAction::Start, &container);
        let (health, health_rx) = tokio::sync::watch::channel(false);
        let (output_complete, output_completion) = tokio::sync::watch::channel(false);
        self.live.lock().await.insert(
            container.id.clone(),
            Run {
                process: Arc::clone(&process),
                health,
                output_complete: output_completion,
            },
        );
        self.waiters
            .lock()
            .await
            .entry(container.id.clone())
            .or_insert_with(|| Arc::new(Notify::new()));
        let service = Arc::clone(self);
        let generation = container.generation;
        if let Some(check) = container.spec.healthcheck.clone() {
            tokio::spawn(
                crate::service::health::Monitor::new(
                    Arc::clone(self),
                    container.id.clone(),
                    generation,
                    check,
                    health_rx,
                )
                .run(),
            );
        }
        let id = container.id;
        let journal = JournalId::container(id.clone());
        let owner_service = Arc::clone(&service);
        let owner_journal = journal.clone();
        // The generation this launch opened, kept so the completion path can close it. `own` no
        // longer does: the stream must not end before the exit status is recorded.
        let terminal = Arc::clone(&io);
        let owner = tokio::spawn(async move { owner_service.own(process, owner_journal, io, output_complete).await });
        let output_owner = Arc::new(super::OutputOwner {
            abort: owner.abort_handle(),
        });
        self.output_owners
            .lock()
            .await
            .insert(journal.clone(), Arc::clone(&output_owner));
        tokio::spawn(async move {
            let result = owner
                .await
                .map_err(|error| Error::Runtime(format!("process output owner failed: {error}")))
                .and_then(std::convert::identity);
            service.retire_output_owner(&journal, &output_owner).await;
            service.finish(id, generation, result).await;
            // `finish` closes this generation on the paths that publish an exit; this covers the
            // ones that return early, so an attached session is never left waiting on a dead process.
            service.retire_io_generation(&journal, &terminal).await;
        });
        Ok(())
    }

    pub(super) async fn launch_networks(&self, container: &Container) -> Result<Vec<NetworkConfig>> {
        self.networks
            .launch(&container.id, container.spec.isolation, container.spec.network_mode)
            .await
    }

    async fn ensure_ports_available(&self, owner: &Container) -> Result<()> {
        let containers = self.containers.list().await?;
        for candidate in &owner.spec.publish {
            let conflict = containers.iter().any(|container| {
                container.id != owner.id
                    && container.state.is_active()
                    && container
                        .spec
                        .publish
                        .iter()
                        .any(|publish| candidate.conflicts(*publish))
            });
            if conflict {
                return Err(Error::PortConflict(candidate.host_ip, candidate.host));
            }
        }
        Ok(())
    }

    pub(super) async fn live(&self, container: &Container) -> Result<Arc<dyn Running>> {
        self.live
            .lock()
            .await
            .get(&container.id)
            .map(|run| Arc::clone(&run.process))
            .ok_or_else(|| Error::Corrupt(format!("active container {} has no owned process", container.id)))
    }
}
