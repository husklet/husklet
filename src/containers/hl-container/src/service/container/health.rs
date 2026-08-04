use super::{
    now_ms, Arc, Check, Container, ContainerId, ContainerState, Duration, ExitStatus, Healthcheck,
    JournalId, Probe, ProcessConfig, Result, Running, Service, Signal,
};

impl Service {
    pub(in crate::service) async fn execute_probe(
        &self,
        id: &ContainerId,
        generation: u64,
        check: &Healthcheck,
    ) -> Probe {
        let started_at_ms = now_ms();
        let process = {
            let _guard = self.operations.lock().await;
            let container = match self.required(id).await {
                Ok(container)
                    if container.generation == generation
                        && matches!(container.state, ContainerState::Running { .. }) =>
                {
                    container
                }
                _ => {
                    return Probe::new(
                        started_at_ms,
                        now_ms(),
                        ExitStatus::Fault {
                            status: -1,
                            detail: 0,
                        },
                        "health generation is no longer running",
                    );
                }
            };
            match self.launch_probe(&container, check).await {
                Ok(process) => process,
                Err(error) => return Self::failed_probe(started_at_ms, error),
            }
        };
        Self::finish_probe(process, check, started_at_ms).await
    }

    async fn launch_probe(
        &self,
        container: &Container,
        check: &Healthcheck,
    ) -> Result<Arc<dyn Running>> {
        let networks = self
            .networks
            .launch(
                &container.id,
                container.spec.isolation,
                container.spec.network_mode,
            )
            .await?;
        let process = Self::health_process(container, check);
        let requested_mounts = container.spec.mounts.clone();
        let (rootfs, overlay, owners) = self.rootfs_launch(&container.spec.rootfs).await?;
        let mut mounts = self.volumes.resolve(&requested_mounts).await?;
        mounts.extend(self.identity.open(container)?);
        let filesystem_generation = self.identity.generation(container)?.path().to_owned();
        let domain = self.process_domain(&container.id).await?;

        self.runtime
            .start(ProcessConfig {
                network_namespace: container.id.namespace(),
                rootfs,
                overlay,
                owners,
                filesystem_generation,
                translation_cache: self.translation_cache.clone(),
                checkpoint: None,
                guest: container.spec.guest,
                process,
                hostname: Some(container.hostname()),
                mounts,
                resources: container.spec.resources,
                isolation: container.spec.isolation,
                network_mode: container.spec.network_mode,
                networks,
                publish: Vec::new(),
                input: None,
                terminal: None,
                domain: Some(domain),
                domain_owner: false,
            })
            .await
    }

    async fn finish_probe(
        process: Arc<dyn Running>,
        check: &Healthcheck,
        started_at_ms: u64,
    ) -> Probe {
        let mut logs = process.take_logs();
        let output = async move {
            let mut bytes = Vec::new();
            if let Some(logs) = logs.as_mut() {
                while let Some(chunk) = logs.recv().await {
                    bytes.extend_from_slice(&chunk.bytes);
                }
            }
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let waiting = Arc::clone(&process).wait();
        tokio::pin!(waiting);
        let result = tokio::select! {
            result = &mut waiting => result.unwrap_or(ExitStatus::Fault { status: -1, detail: 0 }),
            () = tokio::time::sleep(check.timeout) => {
                let _ = process.signal(Signal::Kill).await;
                let _ = waiting.await;
                ExitStatus::Fault { status: -1, detail: 0 }
            }
        };
        Probe::new(started_at_ms, now_ms(), result, output.await)
    }

    pub(in crate::service) async fn record_probe(
        &self,
        id: &ContainerId,
        generation: u64,
        check: &Healthcheck,
        elapsed: Duration,
        probe: Probe,
    ) -> bool {
        let _guard = self.operations.lock().await;
        let Ok(mut container) = self.required(id).await else {
            return false;
        };
        if container.generation != generation
            || !matches!(container.state, ContainerState::Running { .. })
        {
            return false;
        }
        let previous = container.health.as_ref().map(|health| health.status);
        let health = container.health.get_or_insert_with(crate::Health::starting);
        health.record(probe, check, elapsed);
        let changed = previous != Some(health.status);
        if let Err(error) = self.containers.replace(&container).await {
            self.failures
                .lock()
                .await
                .insert(JournalId::container(id.clone()), error.to_string());
            return false;
        }
        if changed {
            self.emit(crate::LifecycleAction::HealthStatus, &container);
        }
        true
    }

    fn health_process(container: &Container, check: &Healthcheck) -> crate::Process {
        let mut process = container.spec.process.clone();
        process.console = crate::Console::default();
        match &check.command {
            Check::Command(command) => {
                process.program.clone_from(&command.program);
                process.args.clone_from(&command.args);
                process.env.extend(command.env.clone());
                if command.uid.is_some() {
                    process.uid = command.uid;
                }
                if command.gid.is_some() {
                    process.gid = command.gid;
                }
            }
            Check::Shell(command) => {
                process.program = "/bin/sh".into();
                process.args = vec!["-c".into(), command.clone()];
            }
        }
        process
    }

    fn failed_probe(started_at_ms: u64, error: impl std::fmt::Display) -> Probe {
        Probe::new(
            started_at_ms,
            now_ms(),
            ExitStatus::Fault {
                status: -1,
                detail: 0,
            },
            error.to_string(),
        )
    }
}
