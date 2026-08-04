use super::{
    Arc, Container, ContainerState, Error, JournalId, NetworkConfig, Notify, ProcessConfig, Result, Run, Running,
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
        let container = self.resolve(reference).await?;
        if container.state.is_active() {
            return Err(Error::AlreadyRunning(container.id));
        }
        self.launch_locked(container, true).await
    }

    #[allow(clippy::too_many_lines, reason = "container launch transaction and rollback")]
    pub(super) async fn launch_locked(self: &Arc<Self>, mut container: Container, explicit: bool) -> Result<()> {
        self.ensure_ports_available(&container).await?;
        self.networks.attach_default_for_publication_locked(&container).await?;
        let networks = self.launch_networks(&container).await?;
        let io = self.io(&container).await;
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
        let checkpoint = Some(CheckpointConfig {
            image: self
                .checkpoints
                .open(&checkpoint_namespace)
                .map_err(|error| Error::Runtime(error.to_string()))?,
            restore: container.checkpoint.is_some(),
        });
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
                resources: container.spec.resources,
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
                self.io.lock().await.remove(&JournalId::container(container.id.clone()));
                return Err(error);
            }
        };
        container.generation = container.generation.saturating_add(1);
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
            if let Err(cleanup) = process.signal(Signal::Kill).await {
                return Err(Error::Runtime(format!(
                    "failed to persist start ({error}); process cleanup also failed ({cleanup})"
                )));
            }
            return Err(error);
        }
        self.emit(crate::LifecycleAction::Start, &container);
        let (health, health_rx) = tokio::sync::watch::channel(false);
        self.live.lock().await.insert(
            container.id.clone(),
            Run {
                process: Arc::clone(&process),
                health,
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
        tokio::spawn(async move {
            let id = container.id;
            let journal = JournalId::container(id.clone());
            let result = Arc::clone(&service).own(process, journal).await;
            service.finish(id, generation, result).await;
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
