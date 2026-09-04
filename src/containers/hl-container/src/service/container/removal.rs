use super::{Container, Error, ExitStatus, JournalId, Result, Rootfs, Service, Signal, WaitCondition};

/// A forced removal is the last teardown step, so its waits are bounded: a guest that never
/// publishes a terminal state must surface as a named failure rather than block the caller forever.
pub(crate) const FORCE_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl Service {
    async fn bounded<T>(id: &crate::ContainerId, operation: impl Future<Output = Result<T>>) -> Result<T> {
        match tokio::time::timeout(FORCE_STOP_TIMEOUT, operation).await {
            Ok(result) => result,
            Err(_) => Err(Error::StopTimeout {
                id: id.clone(),
                seconds: FORCE_STOP_TIMEOUT.as_secs(),
            }),
        }
    }

    pub(crate) async fn remove(
        &self,
        reference: &str,
        force: bool,
        volumes: bool,
        completion: Option<ExitStatus>,
    ) -> Result<Container> {
        if force {
            let container = self.resolve(reference).await?;
            if container.state.is_active() {
                if let Err(error) = self.stop_signal(reference, Signal::KILL).await
                    && !matches!(&error, Error::InvalidState { .. })
                {
                    return Err(error);
                }
                Self::bounded(&container.id, self.wait(reference, WaitCondition::NotRunning)).await?;
            }
            Self::bounded(&container.id, self.stop_and_wait_executions(&container.id)).await?;
        }
        let _guard = self.operations.lock().await;
        let container = self.resolve(reference).await?;
        if container.state.is_active() {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "created or exited",
            });
        }
        let execs = self
            .execs
            .list()
            .await?
            .into_iter()
            .filter(|exec| exec.container == container.id)
            .collect::<Vec<_>>();
        if let Some(exec) = execs.iter().find(|exec| exec.state.is_active()) {
            return Err(Error::InvalidExecState {
                id: exec.id.clone(),
                actual: exec.state.clone(),
                expected: "exited before parent removal",
            });
        }
        self.networks.disconnect_container_locked(&container.id).await?;
        self.identity.remove(&container)?;
        self.containers.remove(&container.id).await?;
        self.emit(crate::LifecycleAction::Destroy, &container);
        for name in container.spec.mounts.iter().filter_map(|mount| match &mount.source {
            crate::MountSource::Tmpfs(name) => Some(name.as_str()),
            crate::MountSource::Anonymous(name) if volumes => Some(name.as_str()),
            crate::MountSource::Anonymous(_) | crate::MountSource::Bind(_) | crate::MountSource::Volume(_) => None,
        }) {
            self.volumes.remove_locked(name).await?;
        }
        self.logs.remove(&JournalId::container(container.id.clone())).await?;
        for exec in &execs {
            let journal = JournalId::exec(exec.id.clone());
            self.logs.remove(&journal).await?;
            self.io.lock().await.remove(&journal);
            self.failures.lock().await.remove(&journal);
        }
        self.execs.remove_parent(&container.id).await?;
        if let Rootfs::Image(reference) = &container.spec.rootfs {
            let manager = self.rootfs.clone().ok_or_else(|| {
                Error::Corrupt("container has an image rootfs but no rootfs manager is configured".into())
            })?;
            let reference = reference.clone();
            let released = tokio::task::spawn_blocking(move || manager.release(&reference))
                .await
                .map_err(|error| Error::Io(std::io::Error::other(error)))?;
            match released {
                Ok(()) | Err(hl_images::Error::NotOwned { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(result) = completion {
            let mut exits = self.exits.lock().await;
            exits.insert(container.id.to_string(), result);
            if let Some(name) = &container.spec.name {
                exits.insert(name.clone(), result);
            }
        }
        if let Some(notify) = self.waiters.lock().await.remove(&container.id) {
            notify.notify_waiters();
        }
        self.failures
            .lock()
            .await
            .remove(&JournalId::container(container.id.clone()));
        if let Some(io) = self.io.lock().await.remove(&JournalId::container(container.id.clone())) {
            io.finish().await;
        }
        Ok(container)
    }

    /// Force-stops every attached execution and waits until each completion task
    /// has published its durable terminal state. This runs without the operation
    /// lock because completion needs that lock to finish ownership teardown.
    async fn stop_and_wait_executions(&self, container: &crate::ContainerId) -> Result<()> {
        let executions = self
            .execs
            .list()
            .await?
            .into_iter()
            .filter(|exec| &exec.container == container && exec.state.is_active())
            .map(|exec| exec.id)
            .collect::<Vec<_>>();
        let mut failure = None;
        for id in &executions {
            let process = self.exec_live.lock().await.get(id).cloned();
            let result = match process {
                Some(process) => process.signal(Signal::KILL).await,
                None => Err(Error::Runtime(format!("running exec {id} has no runtime process"))),
            };
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        for id in executions {
            if let Err(error) = self.wait_exec(&id).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(crate) async fn prune(&self, selection: &crate::Prune) -> Result<Vec<Container>> {
        let candidates = self
            .list()
            .await?
            .into_iter()
            .filter(|container| !container.state.is_active() && selection.matches(container))
            .map(|container| container.id.to_string())
            .collect::<Vec<_>>();
        let mut removed = Vec::with_capacity(candidates.len());
        for id in candidates {
            match self.remove(&id, false, false, None).await {
                Ok(container) => removed.push(container),
                Err(Error::InvalidState { .. } | Error::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(removed)
    }
}
