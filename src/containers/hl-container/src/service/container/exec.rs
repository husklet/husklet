use super::{
    Arc, Error, Exec, ExecId, ExecSpec, ExecState, ExitStatus, Io, JournalId, ProcessConfig, Result, Service, Signal,
    now_ms,
};
use crate::service::CheckpointConfig;

impl Service {
    pub(crate) async fn create_exec(&self, reference: &str, mut spec: ExecSpec) -> Result<Exec> {
        spec.process.validate()?;
        if spec.privileged {
            return Err(Error::InvalidSpec(
                "privileged exec is not implemented by the engine".into(),
            ));
        }
        let _guard = self.operations.lock().await;
        let container = self.resolve(reference).await?;
        container.require_exec()?;
        // A named user only resolves against the container's own root filesystem, so this must
        // happen after the container is known. For an overlay this is the same lower directory
        // container create resolves against, keeping create and exec identities identical.
        let rootfs = self.rootfs_path(&container.spec.rootfs).await?;
        spec.apply_user(&rootfs)?;
        let exec = Exec::new(container.id, spec);
        self.execs.insert(&exec).await?;
        Ok(exec)
    }

    pub(crate) async fn inspect_exec(&self, id: &ExecId) -> Result<Exec> {
        self.execs.get(id).await?.ok_or_else(|| Error::ExecNotFound(id.clone()))
    }

    pub(crate) async fn wait_exec(&self, id: &ExecId) -> Result<ExitStatus> {
        loop {
            let notified = {
                let mut waiters = self.exec_waiters.lock().await;
                Arc::clone(
                    waiters
                        .entry(id.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Notify::new())),
                )
                .notified_owned()
            };
            if let Some(error) = self.failures.lock().await.get(&JournalId::exec(id.clone())) {
                return Err(Error::Runtime(error.clone()));
            }
            match self.inspect_exec(id).await?.state {
                ExecState::Exited { result, .. } => return Ok(result),
                ExecState::Created | ExecState::Running { .. } => {}
            }
            notified.await;
        }
    }

    pub(crate) async fn list_execs(&self) -> Result<Vec<Exec>> {
        self.execs.list().await
    }

    pub(crate) async fn start_exec(
        self: &Arc<Self>,
        id: &ExecId,
        size: Option<crate::Size>,
        claim_attachment: bool,
    ) -> Result<crate::Session> {
        let _guard = self.operations.lock().await;
        let mut exec = self.inspect_exec(id).await?;
        if !matches!(exec.state, ExecState::Created) {
            return Err(Error::InvalidExecState {
                id: exec.id,
                actual: exec.state,
                expected: "exec has not been started",
            });
        }
        if self.exec_live.lock().await.contains_key(id) {
            return Err(Error::Runtime(format!(
                "exec {id} is quarantined while its previous process is being reaped"
            )));
        }
        if let Some(size) = size {
            if exec.spec.process.console.terminal.is_none() {
                return Err(Error::NoTerminal(exec.id.to_string()));
            }
            exec.spec.process.console.terminal = Some(size);
        }
        let container = self.required(&exec.container).await?;
        container.require_exec()?;
        let networks = self.launch_networks(&container).await?;
        let journal = JournalId::exec(exec.id.clone());
        let process_spec = exec.spec.process.clone();
        let requested_mounts = container.spec.mounts.clone();
        let (rootfs, overlay, owners) = self.rootfs_launch(&container.spec.rootfs).await?;
        let mut mounts = self.volumes.resolve(&requested_mounts).await?;
        mounts.extend(self.identity.open(&container)?);
        let filesystem_generation = self.identity.generation(&container)?.path().to_owned();
        let domain = Some(self.process_domain(&container.id).await?);
        let checkpoint = CheckpointConfig {
            image: self
                .checkpoints
                .open(&format!("exec-{}", exec.id))
                .map_err(Error::Checkpoint)?,
            restore: exec.checkpoint.is_some(),
        };
        let (cursor, live_at) = if let Some(cursor) = exec.attachment_cursor {
            (cursor, self.logs.cursor(&journal).await?)
        } else {
            (0, 0)
        };
        // Input ownership is the only destructive preparation step. Keep it
        // after every fallible filesystem, volume, network, identity, domain,
        // and checkpoint lookup so a repaired dependency can be retried.
        let io = self.exec_io(&exec).await;
        let session = crate::Session::new(Arc::clone(self), Arc::clone(&io), journal.clone(), cursor, live_at);
        let session = if claim_attachment {
            session.claim_attachment()?
        } else {
            session
        };
        if exec.checkpoint.is_some() {
            io.rearm_input().await;
        }
        let input = io.take_input().await?;
        let process = self
            .runtime
            .start(ProcessConfig {
                network_namespace: container.id.namespace(),
                rootfs,
                overlay,
                owners,
                filesystem_generation,
                translation_cache: self.translation_cache.clone(),
                checkpoint: Some(checkpoint),
                guest: container.spec.guest,
                execution: container.spec.execution,
                process: process_spec,
                hostname: Some(container.hostname()),
                mounts,
                resources: container.spec.resources,
                isolation: container.spec.isolation,
                network_mode: container.spec.network_mode,
                networks,
                publish: Vec::new(),
                input,
                terminal: exec.spec.process.console.terminal,
                domain,
                domain_owner: false,
            })
            .await;
        let process = match process {
            Ok(process) => process,
            Err(error) => {
                if let Some(io) = self.io.lock().await.remove(&journal) {
                    io.finish().await;
                }
                return Err(error);
            }
        };
        let process_id = process.id();
        let started_at_ms = now_ms();
        exec.state = ExecState::Running {
            process_id,
            started_at_ms,
        };
        exec.checkpoint = None;
        if let Err(error) = self.execs.replace(&exec).await {
            return Err(self
                .rollback_unpublished_exec(exec.id.clone(), process, &journal, error)
                .await);
        }
        self.exec_live
            .lock()
            .await
            .insert(exec.id.clone(), Arc::clone(&process));
        let service = Arc::clone(self);
        let exec_id = exec.id;
        tokio::spawn(async move {
            let result = Arc::clone(&service).own(process, journal).await;
            service.finish_exec(exec_id, process_id, started_at_ms, result).await;
        });
        Ok(session)
    }

    async fn rollback_unpublished_exec(
        self: &Arc<Self>,
        id: ExecId,
        process: Arc<dyn crate::service::Running>,
        journal: &JournalId,
        publication: Error,
    ) -> Error {
        let mut cleanup = Vec::new();
        if let Err(error) = process.signal(Signal::KILL).await {
            cleanup.push(format!("kill failed: {error}"));
        }
        let mut wait = tokio::spawn(Arc::clone(&process).wait());
        let reaped = match tokio::time::timeout(unpublished_reap_timeout(), &mut wait).await {
            Ok(Ok(Ok(_))) => true,
            Ok(Ok(Err(error))) => {
                cleanup.push(format!("reap failed: {error}"));
                false
            }
            Ok(Err(error)) => {
                cleanup.push(format!("reap task failed: {error}"));
                false
            }
            Err(_) => {
                cleanup.push(format!("reap timed out after {:?}", unpublished_reap_timeout()));
                self.exec_live.lock().await.insert(id.clone(), Arc::clone(&process));
                let service = Arc::downgrade(self);
                let journal = journal.clone();
                tokio::spawn(async move {
                    let result = wait.await;
                    let Some(service) = service.upgrade() else {
                        return;
                    };
                    let _guard = service.operations.lock().await;
                    match result {
                        Ok(Ok(_)) => {
                            let owned = service
                                .exec_live
                                .lock()
                                .await
                                .get(&id)
                                .is_some_and(|candidate| Arc::ptr_eq(candidate, &process));
                            if owned {
                                service.exec_live.lock().await.remove(&id);
                                let io = service.io.lock().await.remove(&journal);
                                if let Some(io) = io {
                                    io.finish().await;
                                }
                                if let Some(waiters) = service.exec_waiters.lock().await.get(&id) {
                                    waiters.notify_waiters();
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            hl_log::hl_error!(
                                hl_log::tag::CONTAINER,
                                "quarantined exec reap failed id={} error={error}",
                                id
                            );
                        }
                        Err(error) => {
                            hl_log::hl_error!(
                                hl_log::tag::CONTAINER,
                                "quarantined exec reap task failed id={} error={error}",
                                id
                            );
                        }
                    }
                });
                return Error::Runtime(format!(
                    "exec state publication failed: {publication}; rollback cleanup failed: {}",
                    cleanup.join("; ")
                ));
            }
        };
        if !reaped {
            self.exec_live.lock().await.insert(id.clone(), Arc::clone(&process));
        } else {
            let io = self.io.lock().await.remove(journal);
            if let Some(io) = io {
                io.finish().await;
            }
        }
        if cleanup.is_empty() {
            publication
        } else {
            Error::Runtime(format!(
                "exec state publication failed: {publication}; rollback cleanup failed: {}",
                cleanup.join("; ")
            ))
        }
    }

    pub(crate) async fn attach_exec(
        self: &Arc<Self>,
        id: &ExecId,
        size: Option<crate::Size>,
    ) -> Result<crate::Session> {
        let _guard = self.operations.lock().await;
        let mut exec = self.inspect_exec(id).await?;
        if !matches!(exec.state, ExecState::Running { .. }) {
            return Err(Error::InvalidExecState {
                id: exec.id,
                actual: exec.state,
                expected: "running",
            });
        }
        let journal = JournalId::exec(exec.id.clone());
        let live_at = self.logs.cursor(&journal).await?;
        let io = self
            .io
            .lock()
            .await
            .get(&journal)
            .cloned()
            .ok_or_else(|| Error::Runtime(format!("running exec {id} has no live I/O")))?;
        let cursor = exec.attachment_cursor.unwrap_or(live_at).max(io.delivered_cursor());
        let session = crate::Session::new(Arc::clone(self), io, journal, cursor, live_at).claim_attachment()?;
        if let Some(size) = size {
            let Some(previous) = exec.spec.process.console.terminal else {
                return Err(Error::NoTerminal(exec.id.to_string()));
            };
            let process = self
                .exec_live
                .lock()
                .await
                .get(&exec.id)
                .cloned()
                .ok_or_else(|| Error::Runtime("running exec has no runtime process".into()))?;
            process.resize(size).await?;
            exec.spec.process.console.terminal = Some(size);
            if let Err(error) = self.execs.replace(&exec).await {
                let _ = process.resize(previous).await;
                return Err(error);
            }
        }
        Ok(session)
    }

    async fn finish_exec(&self, id: ExecId, owner: u64, generation: u64, result: Result<ExitStatus>) {
        let _guard = self.operations.lock().await;
        let (result, mut failure) = match result {
            Ok(result) => (result, None),
            Err(error) => (
                ExitStatus::Fault {
                    status: -1,
                    detail: 0,
                    reason: crate::FaultCause::Unknown,
                },
                Some(error.to_string()),
            ),
        };
        if let Ok(mut exec) = self.inspect_exec(&id).await {
            if exec.checkpoint.is_some() {
                return;
            }
            let process_id = match exec.state {
                ExecState::Running {
                    process_id,
                    started_at_ms,
                } if process_id == owner && started_at_ms == generation => Some(process_id),
                ExecState::Running { .. } | ExecState::Created | ExecState::Exited { .. } => return,
            };
            exec.state = ExecState::Exited {
                result,
                finished_at_ms: now_ms(),
                process_id,
            };
            if let Err(error) = self.execs.replace(&exec).await {
                hl_log::hl_error!(
                    hl_log::tag::CONTAINER,
                    "exec completion persistence failed id={} error={error}",
                    id
                );
                let persistence = format!("exec completion persistence failed: {error}");
                failure = Some(match failure {
                    Some(runtime) => format!("{runtime}; {persistence}"),
                    None => persistence,
                });
            }
        }
        if let Some(error) = failure {
            self.failures.lock().await.insert(JournalId::exec(id.clone()), error);
        }
        self.exec_live.lock().await.remove(&id);
        if let Some(waiters) = self.exec_waiters.lock().await.get(&id) {
            waiters.notify_waiters();
        }
        if let Some(io) = self.io.lock().await.remove(&JournalId::exec(id)) {
            io.finish().await;
        }
    }

    pub(crate) async fn checkpoint_execs(self: &Arc<Self>, timeout: std::time::Duration) -> Result<()> {
        let ids = self.checkpointable_execs().await?;
        let mut captured = Vec::new();
        for id in ids {
            let result = async {
                let _guard = self.operations.lock().await;
                let mut exec = self.inspect_exec(&id).await?;
                let process = self
                    .exec_live
                    .lock()
                    .await
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| Error::Runtime(format!("running exec {id} has no runtime process")))?;
                process.checkpoint(timeout).await?;
                let journal = JournalId::exec(id.clone());
                let delivered = self
                    .io
                    .lock()
                    .await
                    .get(&journal)
                    .ok_or_else(|| Error::Runtime(format!("running exec {id} has no live I/O")))?
                    .delivered_cursor();
                exec.attachment_cursor = Some(exec.attachment_cursor.unwrap_or(0).max(delivered));
                let checkpoint = crate::Checkpoint {
                    namespace: format!("exec-{id}"),
                    created_at_ms: now_ms(),
                };
                exec.state = ExecState::Created;
                exec.checkpoint = Some(checkpoint);
                self.execs.replace(&exec).await?;
                self.exec_live.lock().await.remove(&id);
                if let Some(waiters) = self.exec_waiters.lock().await.get(&id) {
                    waiters.notify_waiters();
                }
                Ok(())
            };
            if let Err(mut failure) = result.await {
                for captured_id in captured {
                    if let Err(rollback) = self.start_exec(&captured_id, None, false).await {
                        failure = Error::Runtime(format!(
                            "{failure}; checkpoint rollback failed for terminal {captured_id}: {rollback}"
                        ));
                    }
                }
                return Err(failure);
            }
            captured.push(id);
        }
        let finished = {
            let mut ios = self.io.lock().await;
            captured
                .into_iter()
                .filter_map(|id| ios.remove(&JournalId::exec(id)))
                .collect::<Vec<_>>()
        };
        for io in finished {
            io.finish().await;
        }
        Ok(())
    }

    pub(crate) async fn checkpointable_execs(&self) -> Result<Vec<crate::ExecId>> {
        let ids = self
            .execs
            .list()
            .await?
            .into_iter()
            .filter(|exec| exec.state.is_active())
            .map(|exec| exec.id)
            .collect::<Vec<_>>();
        for id in &ids {
            let process = self
                .exec_live
                .lock()
                .await
                .get(id)
                .cloned()
                .ok_or_else(|| Error::Runtime(format!("running exec {id} has no runtime process")))?;
            if !process.checkpointable() {
                return Err(Error::Runtime(format!(
                    "running terminal {id} is not checkpointable by its runtime"
                )));
            }
        }
        Ok(ids)
    }

    pub(crate) async fn resize_exec(&self, id: &ExecId, size: crate::Size) -> Result<()> {
        let _guard = self.operations.lock().await;
        let mut exec = self.inspect_exec(id).await?;
        if !matches!(exec.state, ExecState::Running { .. }) {
            return Err(Error::InvalidExecState {
                id: exec.id,
                actual: exec.state,
                expected: "running",
            });
        }
        let Some(previous) = exec.spec.process.console.terminal else {
            return Err(Error::NoTerminal(exec.id.to_string()));
        };
        let process = self
            .exec_live
            .lock()
            .await
            .get(&exec.id)
            .cloned()
            .ok_or_else(|| Error::Runtime("running exec has no runtime process".into()))?;
        process.resize(size).await?;
        exec.spec.process.console.terminal = Some(size);
        if let Err(error) = self.execs.replace(&exec).await {
            let _ = process.resize(previous).await;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn signal_exec(&self, id: &ExecId, signal: Signal) -> Result<()> {
        let _guard = self.operations.lock().await;
        let exec = self.inspect_exec(id).await?;
        if !matches!(exec.state, ExecState::Running { .. }) {
            return Err(Error::InvalidExecState {
                id: exec.id,
                actual: exec.state,
                expected: "running",
            });
        }
        let process = self
            .exec_live
            .lock()
            .await
            .get(&exec.id)
            .cloned()
            .ok_or_else(|| Error::Runtime("running exec has no runtime process".into()))?;
        process.signal(signal).await
    }

    pub(crate) async fn remove_exec(&self, id: &ExecId) -> Result<()> {
        let _guard = self.operations.lock().await;
        let exec = self.inspect_exec(id).await?;
        let live = self.exec_live.lock().await.contains_key(&exec.id);
        if live {
            return Err(Error::InvalidExecState {
                id: exec.id,
                actual: exec.state,
                expected: "not running",
            });
        }
        let journal = JournalId::exec(id.clone());
        if !matches!(exec.state, ExecState::Created) {
            match self.logs.remove(&journal).await {
                Ok(()) | Err(Error::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        self.io.lock().await.remove(&journal);
        self.failures.lock().await.remove(&journal);
        self.exec_waiters.lock().await.remove(id);
        self.execs.remove(id).await
    }

    async fn exec_io(&self, exec: &Exec) -> Arc<Io> {
        let mut values = self.io.lock().await;
        let id = JournalId::exec(exec.id.clone());
        if let Some(io) = values.get(&id) {
            return Arc::clone(io);
        }
        let io = Arc::new(Io::new(exec.spec.streams.stdin));
        values.insert(id, Arc::clone(&io));
        io
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
