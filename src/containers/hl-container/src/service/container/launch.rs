use super::{
    Arc, Container, ContainerState, Error, Io, JournalId, NetworkConfig, Notify, ProcessConfig, Result, Run, Running,
    Service, Signal, now_ms,
};
use crate::service::CheckpointConfig;

impl Service {
    pub(super) async fn process_domain(&self, container: &crate::ContainerId) -> Result<hl_engine::Domain> {
        self.live
            .lock()
            .await
            .get(container)
            .map(|run| run.process.domain())
            .ok_or_else(|| crate::Error::Corrupt(format!("running container {container} has no live process domain")))
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
        });
        Ok(())
    }

    async fn rollback_unpublished_launch(
        self: &Arc<Self>,
        id: crate::ContainerId,
        process: Arc<dyn Running>,
        journal: &JournalId,
        io: &Arc<Io>,
        publication: Error,
    ) -> Error {
        let mut cleanup = Vec::new();
        if let Err(error) = process.signal(Signal::KILL).await {
            cleanup.push(format!("kill failed: {error}"));
        }
        let mut wait = tokio::spawn(Arc::clone(&process).wait());
        match tokio::time::timeout(unpublished_reap_timeout(), &mut wait).await {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(error))) => {
                let failure = format!("unpublished process reap failed: {error}");
                self.poison_launch_cleanup(id.clone(), failure.clone()).await;
                cleanup.push(failure);
            }
            Ok(Err(error)) => {
                let failure = format!("unpublished reap task failed: {error}");
                self.poison_launch_cleanup(id.clone(), failure.clone()).await;
                cleanup.push(failure);
            }
            Err(_) => {
                cleanup.push(format!("reap timed out after {:?}", unpublished_reap_timeout()));
                let service = Arc::downgrade(self);
                let cleanup_id = id.clone();
                let cleanup_task = tokio::spawn(async move {
                    let result = wait.await;
                    let Some(service) = service.upgrade() else {
                        return;
                    };
                    let _guard = service.operations.lock().await;
                    match result {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            service
                                .poison_launch_cleanup(
                                    cleanup_id.clone(),
                                    format!("unpublished process reap failed: {error}"),
                                )
                                .await;
                        }
                        Err(error) => {
                            service
                                .poison_launch_cleanup(
                                    cleanup_id.clone(),
                                    format!("unpublished reap task failed: {error}"),
                                )
                                .await;
                        }
                    }
                    service.launch_cleanups.lock().await.remove(&cleanup_id);
                });
                self.launch_cleanups
                    .lock()
                    .await
                    .insert(id, cleanup_task.abort_handle());
            }
        }
        self.retire_io_generation(journal, io).await;
        if cleanup.is_empty() {
            publication
        } else {
            Error::Runtime(format!(
                "failed to persist start ({publication}); process cleanup also failed ({})",
                cleanup.join("; ")
            ))
        }
    }

    async fn poison_launch_cleanup(&self, id: crate::ContainerId, failure: String) {
        self.launch_cleanup_failures.lock().await.insert(id, failure);
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

#[cfg(test)]
fn unpublished_reap_timeout() -> std::time::Duration {
    std::time::Duration::from_millis(25)
}

#[cfg(not(test))]
fn unpublished_reap_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(5)
}
