use super::{
    Arc, ContainerState, Duration, Error, ExitStatus, JournalId, Notify, Result, Service, Signal, WaitCondition, now_ms,
};

impl Service {
    pub(crate) async fn wait(&self, reference: &str, condition: WaitCondition) -> Result<Option<ExitStatus>> {
        let mut exit = None;
        let initial = {
            let _operation = self.operations.lock().await;
            match self.resolve(reference).await {
                Ok(container) => container,
                Err(Error::NotFound(_)) => {
                    if let Some(completed) = self.exits.lock().await.get(reference).copied() {
                        return Ok(Some(completed));
                    }
                    return Err(Error::NotFound(reference.into()));
                }
                Err(error) => return Err(error),
            }
        };
        let id = initial.id.clone();
        let initial_generation = initial.generation;
        let initially_active = matches!(
            initial.state,
            ContainerState::Running { .. } | ContainerState::Paused { .. }
        );
        loop {
            let operation = self.operations.lock().await;
            let container = match self.required(&id).await {
                Ok(value) => value,
                Err(Error::NotFound(_)) => {
                    let completed = self.exits.lock().await.get(id.as_str()).copied();
                    if condition == WaitCondition::Removed {
                        return Ok(exit.or(completed));
                    }
                    if completed.is_some() {
                        return Ok(completed);
                    }
                    return Err(Error::NotFound(id.to_string()));
                }
                Err(error) => return Err(error),
            };
            if let Some(message) = self
                .failures
                .lock()
                .await
                .get(&JournalId::container(container.id.clone()))
                .cloned()
            {
                return Err(Error::Corrupt(format!("failed to persist exit state: {message}")));
            }
            if let Some(diagnostic) = &container.runtime_diagnostic {
                return Err(Error::Runtime(diagnostic.message().to_owned()));
            }
            if let ContainerState::Exited { result, .. } = container.state {
                if condition == WaitCondition::NotRunning {
                    return Ok(Some(result));
                }
                exit = Some(result);
                if condition == WaitCondition::NextExit
                    && (container.generation > initial_generation
                        || initially_active && container.generation == initial_generation)
                {
                    return Ok(Some(result));
                }
            } else if let ContainerState::Restarting { result, .. } = container.state {
                if condition == WaitCondition::NextExit
                    && (container.generation > initial_generation
                        || initially_active && container.generation == initial_generation)
                {
                    return Ok(Some(result));
                }
            } else if matches!(container.state, ContainerState::Created) && condition == WaitCondition::NotRunning {
                return Err(Error::InvalidState {
                    id: container.id,
                    actual: ContainerState::Created,
                    expected: "running or exited",
                });
            }
            let notify = {
                let mut waiters = self.waiters.lock().await;
                Arc::clone(waiters.entry(container.id).or_insert_with(|| Arc::new(Notify::new())))
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            drop(operation);
            notified.await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn has_waiter(&self, id: &crate::ContainerId) -> bool {
        self.waiters.lock().await.contains_key(id)
    }

    pub(crate) async fn signal(&self, reference: &str, signal: Signal) -> Result<()> {
        let _guard = self.operations.lock().await;
        let container = self.resolve(reference).await?;
        if !container.state.is_active() {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "running or paused",
            });
        }
        self.live(&container).await?.signal(signal).await
    }

    pub(super) async fn stop_signal(&self, reference: &str, signal: Signal) -> Result<()> {
        let guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        if let ContainerState::Restarting {
            result, finished_at_ms, ..
        } = container.state
        {
            container.restart.manual();
            container.state = ContainerState::Exited { result, finished_at_ms };
            self.containers.replace(&container).await?;
            if let Some(cancel) = self.restarts.lock().await.remove(&container.id) {
                let _ = cancel.send(true);
            }
            if let Some(notify) = self.waiters.lock().await.get(&container.id) {
                notify.notify_waiters();
            }
            return Ok(());
        }
        if !container.state.is_active() {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "running or paused",
            });
        }
        container.restart.manual();
        self.containers.replace(&container).await?;
        let process = self.live(&container).await?;
        drop(guard);
        process.signal(signal).await
    }

    pub(crate) async fn resize(&self, reference: &str, size: crate::Size) -> Result<()> {
        let _guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        if !matches!(
            container.state,
            ContainerState::Running { .. } | ContainerState::Paused { .. }
        ) {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "running or paused",
            });
        }
        let Some(previous) = container.spec.process.console.terminal else {
            return Err(Error::NoTerminal(container.id.to_string()));
        };
        let process = self.live(&container).await?;
        process.resize(size).await?;
        container.spec.process.console.terminal = Some(size);
        if let Err(error) = self.containers.replace(&container).await {
            let _ = process.resize(previous).await;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn pause(self: &Arc<Self>, reference: &str) -> Result<()> {
        let _guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        let ContainerState::Running {
            process_id,
            started_at_ms,
        } = container.state
        else {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "running",
            });
        };
        self.live(&container).await?.pause().await?;
        container.state = ContainerState::Paused {
            process_id,
            started_at_ms,
            paused_at_ms: now_ms(),
        };
        if let Err(error) = self.containers.replace(&container).await {
            let _ = self.live(&container).await?.resume().await;
            return Err(error);
        }
        if let Some(run) = self.live.lock().await.get(&container.id) {
            let _ = run.health.send(true);
        }
        self.emit(crate::LifecycleAction::Pause, &container);
        Ok(())
    }

    pub(crate) async fn unpause(self: &Arc<Self>, reference: &str) -> Result<()> {
        let _guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        let ContainerState::Paused {
            process_id,
            started_at_ms,
            ..
        } = container.state
        else {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "paused",
            });
        };
        self.live(&container).await?.resume().await?;
        container.state = ContainerState::Running {
            process_id,
            started_at_ms,
        };
        if let Err(error) = self.containers.replace(&container).await {
            let _ = self.live(&container).await?.pause().await;
            return Err(error);
        }
        if let Some(check) = container.spec.healthcheck.clone() {
            let (health, health_rx) = tokio::sync::watch::channel(false);
            if let Some(run) = self.live.lock().await.get_mut(&container.id) {
                run.health = health;
            }
            tokio::spawn(
                crate::service::health::Monitor::new(
                    Arc::clone(self),
                    container.id.clone(),
                    container.generation,
                    check,
                    health_rx,
                )
                .run(),
            );
        }
        self.emit(crate::LifecycleAction::Unpause, &container);
        Ok(())
    }

    pub(crate) async fn checkpoint(self: &Arc<Self>, reference: &str, timeout: Duration) -> Result<crate::Checkpoint> {
        let _guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        if !matches!(container.state, ContainerState::Running { .. }) {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "running",
            });
        }
        let process = self.live(&container).await?;
        process.checkpoint(timeout).await?;
        let checkpoint = crate::Checkpoint {
            namespace: container.id.to_string(),
            created_at_ms: now_ms(),
        };
        container.restart.manual();
        container.state = ContainerState::Exited {
            result: ExitStatus::Code(0),
            finished_at_ms: checkpoint.created_at_ms,
        };
        container.checkpoint = Some(checkpoint.clone());
        self.containers.replace(&container).await?;
        if let Some(run) = self.live.lock().await.remove(&container.id) {
            let _ = run.health.send(true);
        }
        if let Some(io) = self.io.lock().await.remove(&JournalId::container(container.id.clone())) {
            io.finish();
        }
        if let Some(notify) = self.waiters.lock().await.get(&container.id) {
            notify.notify_waiters();
        }
        Ok(checkpoint)
    }

    pub(crate) async fn stop(self: &Arc<Self>, reference: &str, timeout: Duration) -> Result<ExitStatus> {
        if self.resolve(reference).await?.state.is_paused() {
            self.unpause(reference).await?;
        }
        let signal = self.resolve(reference).await?.spec.stop_signal;
        self.stop_signal(reference, signal).await?;
        if let Ok(result) = tokio::time::timeout(timeout, self.wait(reference, WaitCondition::NotRunning)).await {
            result?.ok_or_else(|| Error::NotFound(reference.into()))
        } else {
            if let Err(error) = self.stop_signal(reference, Signal::KILL).await
                && !matches!(&error, Error::InvalidState { .. })
            {
                return Err(error);
            }
            self.wait(reference, WaitCondition::NotRunning)
                .await?
                .ok_or_else(|| Error::NotFound(reference.into()))
        }
    }
}
