//! Whole-image capture of a running container and of every container at once.
//!
//! A capture must reach a point where nothing in the domain is running before the image is
//! written, and must put back exactly what it stopped when it cannot. Both the per-container
//! sequence and the all-container sweep with its rollback live here, apart from the ordinary
//! lifecycle verbs beside them.

use super::{Arc, ContainerState, Duration, Error, ExitStatus, JournalId, Result, Service, now_ms};

impl Service {
    pub(crate) async fn checkpoint(self: &Arc<Self>, reference: &str, timeout: Duration) -> Result<crate::Checkpoint> {
        let _guard = self.operations.lock().await;
        self.checkpoint_locked(reference, timeout).await
    }

    pub(super) async fn checkpoint_locked(
        self: &Arc<Self>,
        reference: &str,
        timeout: Duration,
    ) -> Result<crate::Checkpoint> {
        let mut container = self.resolve(reference).await?;
        if !matches!(container.state, ContainerState::Running { .. }) {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "running",
            });
        }
        let (process, output_complete) = {
            let live = self.live.lock().await;
            let run = live
                .get(&container.id)
                .ok_or_else(|| Error::Corrupt(format!("active container {} has no owned process", container.id)))?;
            (Arc::clone(&run.process), run.output_complete.clone())
        };
        // Read before the freeze, while every member is still running: the guest pid is the identity
        // the image will name each sealed member by, and a released member is reaped as soon as the
        // capture commits, taking its handle with it.
        let identities = self.domain_member_identities(&container.id).await?;
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
        let members = self.arm_domain_members(&container.id, &checkpoint, &identities).await?;
        // One deadline governs the container's wait and every member's, so a capture cannot spend
        // `timeout` per journal and outlive the budget its caller reports against.
        let deadline = tokio::time::Instant::now() + timeout;
        let output = self
            .await_output_completion(&JournalId::container(container.id.clone()), output_complete, deadline)
            .await
            .and(self.await_domain_member_stop(members, deadline).await);
        if let Some(run) = self.live.lock().await.remove(&container.id) {
            let _ = run.health.send(true);
        }
        if let Some(io) = self.io.lock().await.remove(&JournalId::container(container.id.clone())) {
            io.finish().await;
        }
        if let Some(notify) = self.waiters.lock().await.get(&container.id) {
            notify.notify_waiters();
        }
        output.map(|()| checkpoint)
    }

    /// Records the container's committed capture against every sealed domain member.
    ///
    /// An exec session is a member of the container's freeze and opens no image of its own, so
    /// its captured state lives inside the container's image and its token names that same
    /// namespace. The token is the durable record that the member was sealed, not a second
    /// artifact: there is exactly one image for the whole process domain.
    ///
    /// This runs under `self.operations`, which [`Self::finish_exec`] also takes before writing a
    /// terminal state, so the token is armed before a released member's `_exit(0)` can be
    /// observed. That ordering is what keeps a clean release distinguishable from a crash.
    /// The container-namespace pid each live member of this container's domain is running under.
    ///
    /// Taken while the members are still running, because that is the only window in which the
    /// runtime holds a handle to ask.
    async fn domain_member_identities(
        &self,
        container: &crate::ContainerId,
    ) -> Result<std::collections::HashMap<crate::ExecId, std::num::NonZeroI32>> {
        let live = self.exec_live.lock().await;
        let mut identities = std::collections::HashMap::new();
        for exec in self.execs.list().await? {
            if &exec.container != container || !exec.state.is_active() {
                continue;
            }
            if let Some(guest_pid) = live.get(&exec.id).and_then(|process| process.guest_pid()) {
                identities.insert(exec.id, guest_pid);
            }
        }
        Ok(identities)
    }

    async fn arm_domain_members(
        &self,
        container: &crate::ContainerId,
        checkpoint: &crate::Checkpoint,
        identities: &std::collections::HashMap<crate::ExecId, std::num::NonZeroI32>,
    ) -> Result<Vec<crate::ExecId>> {
        let mut sealed = Vec::new();
        for mut exec in self.execs.list().await? {
            if &exec.container != container || !exec.state.is_active() {
                continue;
            }
            exec.state = crate::ExecState::Created;
            exec.checkpoint = Some(checkpoint.clone());
            exec.guest_pid = identities.get(&exec.id).copied();
            self.execs.replace(&exec).await?;
            sealed.push(exec.id);
        }
        Ok(sealed)
    }

    /// Waits for every sealed domain member's runtime process to be reaped, and retires its
    /// runtime bookkeeping.
    ///
    /// A committed capture releases every member to exit and is the container's stop, so the
    /// caller's next act is a restore into the SAME network namespace, `SysV` control block and
    /// filesystem generation. Declaring the container `Exited` while a member of the previous
    /// generation is still executing would let those two trees overlap: the restored processes
    /// would find the original container's live IPC control block instead of publishing their
    /// own, and the restored tree would run beside a partially-released one. The container's own
    /// worker is already covered by [`Self::await_output_completion`] -- an output owner only
    /// signals completion after `Running::wait` has returned -- and a member is covered by the
    /// identical signal on its own journal.
    ///
    /// `finish_exec` takes `self.operations`, which this function's caller holds, so the member's
    /// terminal bookkeeping cannot run until the capture returns. Retiring `exec_live` here is
    /// therefore not a duplicate: without it a restore admitted under the same lock would be
    /// quarantined behind an entry describing a process that no longer exists.
    ///
    /// A member that does not stop inside `timeout` fails the capture, which rolls it back. It is
    /// never reported as captured while it is still running.
    /// Members are waited on concurrently under one shared deadline. Sequential per-member waits
    /// at a full budget each made the capture's total wait scale with the member count, so a
    /// workspace with three wedged panes ran three times its own timeout and the GUI's fixed
    /// close budget expired first -- surfacing a bare handover timeout instead of the attributed
    /// journal below. Every member is still waited on and reported; only the arithmetic changed.
    async fn await_domain_member_stop(
        self: &Arc<Self>,
        members: Vec<crate::ExecId>,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        let mut waits = Vec::new();
        for member in members {
            let completion = self.exec_output_complete.lock().await.get(&member).cloned();
            let Some(completion) = completion else {
                continue; // never started, or already reaped and retired
            };
            let service = Arc::clone(self);
            waits.push(tokio::spawn(async move {
                let journal = JournalId::exec(member.clone());
                let result = service.await_output_completion(&journal, completion, deadline).await;
                (member, result)
            }));
        }
        let mut failure = None;
        for wait in waits {
            let outcome = match wait.await {
                Ok(outcome) => outcome,
                Err(error) => {
                    failure.get_or_insert(Error::Runtime(format!("domain member stop wait failed: {error}")));
                    continue;
                }
            };
            match outcome {
                (member, Ok(())) => {
                    self.exec_live.lock().await.remove(&member);
                    self.exec_output_complete.lock().await.remove(&member);
                }
                (_, Err(error)) => {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// The id of a container whose checkpoint is present even though its capture reported failure.
    ///
    /// A container that exited holding a checkpoint was captured, so the rollback below must still
    /// restart it; the error described what happened after the capture, not the capture itself.
    async fn captured_by_exit(&self, reference: &str) -> Option<crate::ContainerId> {
        let container = self.resolve(reference).await.ok()?;
        (container.checkpoint.is_some() && matches!(container.state, ContainerState::Exited { .. }))
            .then_some(container.id)
    }

    pub(crate) async fn checkpoint_all(self: &Arc<Self>, timeout: Duration) -> Result<()> {
        let _guard = self.operations.lock().await;
        #[cfg(test)]
        self.wait_checkpoint_all_gate().await;
        let mut failure = None;
        let mut captured = Vec::new();
        let mut resumed = Vec::new();
        for container in self.containers.list().await? {
            let container_id = container.id.clone();
            let result = match container.state {
                ContainerState::Restarting { .. } => self.cancel_restart_locked(container).await.map(|()| None),
                ContainerState::Created | ContainerState::Exited { .. } => Ok(None),
                ContainerState::Running { .. } => self
                    .checkpoint_locked(container.id.as_str(), timeout)
                    .await
                    .map(|_| Some(container.id)),
                ContainerState::Paused { .. } => match self.unpause_locked(container.id.as_str()).await {
                    Ok(()) => {
                        resumed.push(container.id.clone());
                        self.checkpoint_locked(container.id.as_str(), timeout)
                            .await
                            .map(|_| Some(container.id))
                    }
                    Err(error) => Err(error),
                },
            };
            match result {
                Ok(Some(id)) => captured.push(id),
                Ok(None) | Err(Error::NotFound(_)) => {}
                Err(error) => {
                    captured.extend(self.captured_by_exit(container_id.as_str()).await);
                    failure = failure.or(Some(error));
                }
            }
        }
        let Some(mut failure) = failure else {
            return Ok(());
        };
        for id in captured {
            if let Err(rollback) = self.start_locked(id.as_str()).await {
                failure = Error::Runtime(format!("{failure}; checkpoint rollback failed for {id}: {rollback}"));
            }
        }
        for id in resumed {
            if let Err(rollback) = self.pause_locked(id.as_str()).await {
                failure = Error::Runtime(format!("{failure}; pause rollback failed for {id}: {rollback}"));
            }
        }
        Err(failure)
    }
}
