use super::{
    Arc, ContainerId, ContainerState, Duration, ExecState, ExitStatus, JournalId, Result, Service, Signal, now_ms,
};

impl Service {
    pub(crate) async fn reconcile(&self) -> Result<()> {
        let _guard = self.operations.lock().await;
        for container in self.containers.list().await? {
            self.reconcile_container(container).await?;
        }
        for mut exec in self.execs.list().await? {
            if exec.state.is_active() && exec.checkpoint.is_none() {
                interrupt_exec(&mut exec);
                self.execs.replace(&exec).await?;
            }
        }
        Ok(())
    }

    /// Settles one container's state after a daemon restart interrupted it.
    async fn reconcile_container(&self, mut container: crate::Container) -> Result<()> {
        if matches!(
            container.state,
            ContainerState::Running { .. } | ContainerState::Paused { .. }
        ) {
            let result = ExitStatus::Fault {
                status: -1,
                detail: 0,
                reason: crate::FaultCause::Unknown,
            };
            let finished_at_ms = now_ms();
            container.state = if container.spec.restart.allows_after_daemon_restart() {
                ContainerState::Restarting {
                    result,
                    finished_at_ms,
                    ready_at_ms: finished_at_ms,
                }
            } else {
                ContainerState::Exited { result, finished_at_ms }
            };
            self.containers.replace(&container).await?;
            self.emit(crate::LifecycleAction::Die, &container);
            if matches!(container.state, ContainerState::Restarting { .. }) {
                self.emit(crate::LifecycleAction::Restart, &container);
            }
        } else if let ContainerState::Exited { result, finished_at_ms } = container.state
            && container.spec.restart == crate::RestartPolicy::Always
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
        Ok(())
    }

    pub(crate) async fn recover(self: &Arc<Self>) -> Result<()> {
        // Publish every durable restart before reclaiming exited automatic-removal
        // containers. This preserves daemon-start ordering: unrelated restartable
        // workloads are not delayed behind potentially slow filesystem cleanup.
        for container in self.containers.list().await? {
            let ContainerState::Restarting { ready_at_ms, .. } = container.state else {
                continue;
            };
            let delay = Duration::from_millis(ready_at_ms.saturating_sub(now_ms()));
            let (cancel, mut cancellation) = tokio::sync::watch::channel(false);
            self.restarts.lock().await.insert(container.id.clone(), cancel);
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

        let removals = self
            .containers
            .list()
            .await?
            .into_iter()
            .filter_map(|container| {
                let ContainerState::Exited { result, .. } = container.state else {
                    return None;
                };
                (container.spec.removal == crate::RemovalPolicy::Automatic).then_some((container.id, result))
            })
            .collect::<Vec<_>>();
        let mut failure = None;
        for (id, result) in removals {
            if let Err(error) = self.remove(&id.to_string(), false, true, Some(result)).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) async fn finish(self: &Arc<Self>, id: ContainerId, generation: u64, result: Result<ExitStatus>) {
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
        let (ContainerState::Running { started_at_ms, .. } | ContainerState::Paused { started_at_ms, .. }) =
            container.state
        else {
            unreachable!("active state checked above")
        };
        self.stop_executions(&id).await;
        let (result, diagnostic) = match result {
            Ok(result) => (result, None),
            Err(error) => (
                ExitStatus::Fault {
                    status: -1,
                    detail: 0,
                    reason: crate::FaultCause::Unknown,
                },
                Some(crate::model::RuntimeDiagnostic::new(error.to_string())),
            ),
        };
        let finished_at_ms = now_ms();
        container.restart.completed_between(started_at_ms, finished_at_ms);
        container.runtime_diagnostic = diagnostic;
        let restart = container.spec.restart.allows(result, &container.restart);
        if restart {
            let delay = container.spec.restart.delay(&container.restart);
            container.state = ContainerState::Restarting {
                result,
                finished_at_ms,
                ready_at_ms: finished_at_ms.saturating_add(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)),
            };
        } else {
            container.state = ContainerState::Exited { result, finished_at_ms };
        }
        if let Err(error) = self.containers.replace(&container).await {
            self.failures
                .lock()
                .await
                .insert(JournalId::container(id.clone()), error.to_string());
            return;
        }
        self.emit(crate::LifecycleAction::Die, &container);
        if let Some(notify) = self.waiters.lock().await.get(&id) {
            notify.notify_waiters();
        }
        if restart {
            self.emit(crate::LifecycleAction::Restart, &container);
        }
        let run = self.live.lock().await.remove(&id);
        if let Some(run) = &run {
            let _ = run.health.send(true);
        }
        if let Some(io) = self.io.lock().await.remove(&JournalId::container(id.clone())) {
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
        } else if container.spec.removal == crate::RemovalPolicy::Automatic {
            let reference = id.to_string();
            drop(guard);
            if let Err(error) = self.remove(&reference, false, true, Some(result)).await {
                self.failures
                    .lock()
                    .await
                    .insert(JournalId::container(id), error.to_string());
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
            let Err(error) = self.launch_locked(container.clone(), false).await else {
                return;
            };
            let mut terminal = container;
            terminal.state = ContainerState::Exited {
                result: ExitStatus::Fault {
                    status: -1,
                    detail: 0,
                    reason: crate::FaultCause::Unknown,
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
        })
    }
}

fn interrupt_exec(exec: &mut crate::Exec) {
    let process_id = match exec.state {
        ExecState::Running { process_id, .. } => Some(process_id),
        ExecState::Created | ExecState::Exited { .. } => None,
    };
    exec.state = ExecState::Exited {
        result: ExitStatus::Fault {
            status: -1,
            detail: 0,
            reason: crate::FaultCause::Unknown,
        },
        finished_at_ms: now_ms(),
        process_id,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_retains_the_running_exec_process_id() {
        let mut exec = crate::Exec::new(ContainerId::new(), crate::ExecSpec::new(crate::Process::new("fake")));
        exec.state = ExecState::Running {
            process_id: 73,
            started_at_ms: 100,
        };

        interrupt_exec(&mut exec);

        assert!(matches!(
            exec.state,
            ExecState::Exited {
                result: ExitStatus::Fault {
                    status: -1,
                    detail: 0,
                    reason: crate::FaultCause::Unknown
                },
                process_id: Some(73),
                ..
            }
        ));
    }
}
