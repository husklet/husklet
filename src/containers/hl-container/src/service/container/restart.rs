use super::{
    now_ms, Arc, ContainerId, ContainerState, Duration, ExecState, ExitStatus, JournalId, Result,
    Service, Signal,
};

impl Service {
    pub(crate) async fn reconcile(&self) -> Result<()> {
        let _guard = self.operations.lock().await;
        for mut container in self.containers.list().await? {
            if matches!(
                container.state,
                ContainerState::Running { .. } | ContainerState::Paused { .. }
            ) {
                let result = ExitStatus::Fault {
                    status: -1,
                    detail: 0,
                };
                let finished_at_ms = now_ms();
                container.state = if container.spec.restart.allows(result, &container.restart) {
                    ContainerState::Restarting {
                        result,
                        finished_at_ms,
                        ready_at_ms: finished_at_ms,
                    }
                } else {
                    ContainerState::Exited {
                        result,
                        finished_at_ms,
                    }
                };
                self.containers.replace(&container).await?;
                self.emit(crate::LifecycleAction::Die, &container);
                if matches!(container.state, ContainerState::Restarting { .. }) {
                    self.emit(crate::LifecycleAction::Restart, &container);
                }
            } else if let ContainerState::Exited {
                result,
                finished_at_ms,
            } = container.state
            {
                if container.spec.restart == crate::RestartPolicy::Always
                    && container.restart.manually_stopped
                {
                    container.restart.manually_stopped = false;
                    container.state = ContainerState::Restarting {
                        result,
                        finished_at_ms,
                        ready_at_ms: now_ms(),
                    };
                    self.containers.replace(&container).await?;
                    self.emit(crate::LifecycleAction::Restart, &container);
                }
            }
        }
        for mut exec in self.execs.list().await? {
            if exec.state.is_active() {
                exec.state = ExecState::Exited {
                    result: ExitStatus::Fault {
                        status: -1,
                        detail: 0,
                    },
                    finished_at_ms: now_ms(),
                };
                self.execs.replace(&exec).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn recover(self: &Arc<Self>) -> Result<()> {
        for container in self.containers.list().await? {
            let ContainerState::Restarting { ready_at_ms, .. } = container.state else {
                continue;
            };
            let delay = Duration::from_millis(ready_at_ms.saturating_sub(now_ms()));
            let (cancel, mut cancellation) = tokio::sync::watch::channel(false);
            self.restarts
                .lock()
                .await
                .insert(container.id.clone(), cancel);
            let service = Arc::clone(self);
            let id = container.id;
            let generation = container.generation;
            tokio::spawn(async move {
                tokio::select! {
                    () = tokio::time::sleep(delay) => service.restart(id, generation).await,
                    changed = cancellation.changed() => { let _ = changed; }
                }
            });
        }
        Ok(())
    }

    pub(super) async fn finish(
        self: &Arc<Self>,
        id: ContainerId,
        generation: u64,
        result: Result<ExitStatus>,
    ) {
        let guard = self.operations.lock().await;
        let Ok(mut container) = self.required(&id).await else {
            return;
        };
        if container.generation != generation
            || !matches!(
                container.state,
                ContainerState::Running { .. } | ContainerState::Paused { .. }
            )
        {
            return;
        }
        let (ContainerState::Running { started_at_ms, .. }
        | ContainerState::Paused { started_at_ms, .. }) = container.state
        else {
            unreachable!("active state checked above")
        };
        self.stop_executions(&id).await;
        let result = result.unwrap_or(ExitStatus::Fault {
            status: -1,
            detail: 0,
        });
        let finished_at_ms = now_ms();
        container
            .restart
            .completed_between(started_at_ms, finished_at_ms);
        let restart = container.spec.restart.allows(result, &container.restart);
        if restart {
            let delay = container.spec.restart.delay(&container.restart);
            container.state = ContainerState::Restarting {
                result,
                finished_at_ms,
                ready_at_ms: finished_at_ms
                    .saturating_add(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)),
            };
        } else {
            container.state = ContainerState::Exited {
                result,
                finished_at_ms,
            };
        }
        if let Err(error) = self.containers.replace(&container).await {
            self.failures
                .lock()
                .await
                .insert(JournalId::container(id.clone()), error.to_string());
            return;
        }
        self.emit(crate::LifecycleAction::Die, &container);
        if restart {
            self.emit(crate::LifecycleAction::Restart, &container);
        }
        let run = self.live.lock().await.remove(&id);
        if let Some(run) = &run {
            let _ = run.health.send(true);
        }
        if let Some(io) = self
            .io
            .lock()
            .await
            .remove(&JournalId::container(id.clone()))
        {
            io.finish();
        }
        if restart {
            let Some(_) = run else { return };
            let (cancel, mut cancellation) = tokio::sync::watch::channel(false);
            self.restarts.lock().await.insert(id.clone(), cancel);
            let delay = container.spec.restart.delay(&container.restart);
            let service = Arc::clone(self);
            drop(guard);
            tokio::spawn(async move {
                tokio::select! {
                    () = tokio::time::sleep(delay) => {
                        service.restart(id, generation).await;
                    }
                    changed = cancellation.changed() => {
                        let _ = changed;
                        service.restarts.lock().await.remove(&id);
                    }
                }
            });
        } else {
            let automatic = container.spec.removal == crate::RemovalPolicy::Automatic;
            if automatic {
                let reference = id.to_string();
                drop(guard);
                if let Err(error) = self.remove(&reference, false, true, Some(result)).await {
                    self.failures
                        .lock()
                        .await
                        .insert(JournalId::container(id), error.to_string());
                }
            } else if let Some(notify) = self.waiters.lock().await.get(&id) {
                notify.notify_waiters();
            }
        }
    }

    async fn stop_executions(&self, container: &ContainerId) {
        let children = self
            .execs
            .list()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|exec| &exec.container == container && exec.state.is_active())
            .map(|exec| exec.id)
            .collect::<Vec<_>>();
        for child in children {
            if let Some(process) = self.exec_live.lock().await.get(&child).cloned() {
                let _ = process.signal(Signal::Kill).await;
            }
        }
    }

    fn restart(
        self: &Arc<Self>,
        id: ContainerId,
        generation: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let _guard = self.operations.lock().await;
            self.restarts.lock().await.remove(&id);
            let Ok(container) = self.required(&id).await else {
                return;
            };
            if container.generation != generation
                || container.restart.manually_stopped
                || !matches!(container.state, ContainerState::Restarting { .. })
            {
                return;
            }
            if let Err(error) = self.launch_locked(container.clone(), false).await {
                let mut terminal = container;
                terminal.state = ContainerState::Exited {
                    result: ExitStatus::Fault {
                        status: -1,
                        detail: 0,
                    },
                    finished_at_ms: now_ms(),
                };
                if let Err(persist) = self.containers.replace(&terminal).await {
                    self.failures.lock().await.insert(
                        JournalId::container(id.clone()),
                        format!("restart failed ({error}); exit persistence failed ({persist})"),
                    );
                }
                if let Some(notify) = self.waiters.lock().await.get(&id) {
                    notify.notify_waiters();
                }
            }
        })
    }
}
