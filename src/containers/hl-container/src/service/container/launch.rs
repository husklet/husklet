use super::{
    now_ms, Arc, Container, ContainerState, Error, JournalId, NetworkConfig, Notify, ProcessConfig,
    Result, Run, Running, Service, Signal,
};
use crate::service::CheckpointConfig;

impl Service {
    pub(super) fn devices(
        &self,
        guest: crate::Guest,
        process: &mut crate::Process,
        mounts: &mut Vec<crate::Mount>,
    ) -> Result<crate::DeviceRequest> {
        let request = self
            .devices
            .request(crate::DeviceContext { guest, process })?;
        for device_mount in &request.mounts {
            if mounts
                .iter()
                .any(|mount| mount.target == device_mount.target)
            {
                return Err(Error::InvalidSpec(format!(
                    "device mount conflicts with container mount: {}",
                    device_mount.target.display()
                )));
            }
        }
        mounts.extend(request.mounts.iter().cloned());
        process.env.extend(
            request
                .environment
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        Ok(request)
    }

    pub(crate) async fn start(self: &Arc<Self>, reference: &str) -> Result<()> {
        let _guard = self.operations.lock().await;
        let container = self.resolve(reference).await?;
        if container.state.is_active() {
            return Err(Error::AlreadyRunning(container.id));
        }
        self.launch_locked(container, true).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "container launch transaction and rollback"
    )]
    pub(super) async fn launch_locked(
        self: &Arc<Self>,
        mut container: Container,
        explicit: bool,
    ) -> Result<()> {
        self.ensure_ports_available(&container).await?;
        self.networks
            .attach_default_for_publication_locked(&container)
            .await?;
        let networks = self.launch_networks(&container).await?;
        let io = self.io(&container).await;
        let input = io.take_input().await?;
        let mut process_spec = container.spec.process.clone();
        let mut requested_mounts = container.spec.mounts.clone();
        let devices = self.devices(
            container.spec.guest,
            &mut process_spec,
            &mut requested_mounts,
        )?;
        let mut mounts = self.volumes.resolve(&requested_mounts).await?;
        mounts.extend(self.identity.prepare(&container, &networks)?);
        let filesystem_generation = self.identity.generation(&container)?.path().to_owned();
        let checkpoint_namespace = container.checkpoint.as_ref().map_or_else(
            || container.id.to_string(),
            |checkpoint| checkpoint.namespace.clone(),
        );
        let checkpoint = Some(CheckpointConfig {
            image: self
                .checkpoints
                .open(&checkpoint_namespace)
                .map_err(|error| Error::Runtime(error.to_string()))?,
            restore: container.checkpoint.is_some(),
        });
        let (rootfs, overlay, owners) = self.rootfs_launch(&container.spec.rootfs).await?;
        let process = self
            .runtime
            .start(ProcessConfig {
                network_namespace: container.id.namespace(),
                rootfs,
                overlay,
                owners,
                filesystem_generation,
                checkpoint,
                guest: container.spec.guest,
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
                extensions: devices.extensions,
                authorities: devices.authorities,
            })
            .await;
        let process = match process {
            Ok(process) => process,
            Err(error) => {
                self.io
                    .lock()
                    .await
                    .remove(&JournalId::container(container.id.clone()));
                return Err(error);
            }
        };
        container.generation = container.generation.saturating_add(1);
        container.checkpoint = None;
        container.restart.started(explicit);
        if !explicit {
            container.restart.automatic();
        }
        container.state = ContainerState::Running {
            process_id: process.id(),
            started_at_ms: now_ms(),
        };
        container.health = container
            .spec
            .healthcheck
            .as_ref()
            .map(|_| crate::Health::starting());
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

    pub(super) async fn launch_networks(
        &self,
        container: &Container,
    ) -> Result<Vec<NetworkConfig>> {
        self.networks
            .launch(
                &container.id,
                container.spec.isolation,
                container.spec.network_mode,
            )
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
            .ok_or_else(|| {
                Error::Corrupt(format!(
                    "active container {} has no owned process",
                    container.id
                ))
            })
    }
}
