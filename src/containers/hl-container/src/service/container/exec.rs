use super::{
    Arc, Error, Exec, ExecId, ExecSpec, ExecState, ExitStatus, Io, JournalId, ProcessConfig, Result, Running, Service,
    Signal, now_ms,
};

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
        #[cfg(test)]
        self.exec_start_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        let _guard = self.operations.lock().await;
        self.start_exec_locked(id, size, claim_attachment).await
    }

    async fn start_exec_locked(
        self: &Arc<Self>,
        id: &ExecId,
        size: Option<crate::Size>,
        claim_attachment: bool,
    ) -> Result<crate::Session> {
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
        let (rootfs, overlay, owners, executable_digest_authority) = self.rootfs_launch(&container.spec.rootfs).await?;
        let mut mounts = self.volumes.resolve(&requested_mounts).await?;
        mounts.extend(self.identity.open(&container)?);
        let filesystem_generation = self.identity.generation(&container)?.path().to_owned();
        let domain = Some(self.process_domain(&container.id).await?);
        // An exec session holds the far ends of the container's sockets and pipes, so
        // it is a member of the container's freeze rather than the subject of a
        // capture of its own. It therefore opens no image: there is no `exec-<id>`
        // namespace for a second, invisible generation to be committed into.
        let checkpoint = crate::service::CheckpointRole::DomainMember;
        let (cursor, live_at) = if let Some(cursor) = exec.attachment_cursor {
            (cursor, self.logs.cursor(&journal).await?)
        } else {
            (0, 0)
        };
        // Input ownership is the only destructive preparation step. Keep it
        // after every fallible filesystem, volume, network, identity, domain,
        // and checkpoint lookup so a repaired dependency can be retried.
        let io = self.new_exec_io(&exec, live_at).await?;
        let session = crate::Session::new(Arc::clone(self), Arc::clone(&io), journal.clone(), cursor, live_at);
        let session = if claim_attachment {
            session.claim_attachment()?
        } else {
            session
        };
        let input = io.take_input().await?;
        let process = self
            .runtime
            .start(ProcessConfig {
                network_namespace: container.id.namespace(),
                rootfs,
                overlay,
                executable_digest_authority,
                owners,
                filesystem_generation,
                translation_cache: self.translation_cache.clone(),
                translation_cache_observability: self.translation_cache_observability,
                translation_symbols: self.translation_symbols.clone(),
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
                // An exec launch starts one process and restores nothing, so it revives no members.
                member_terminals: Vec::new(),
                terminal: exec.spec.process.console.terminal,
                domain,
                domain_owner: false,
            })
            .await;
        let process = match process {
            Ok(process) => process,
            Err(error) => {
                self.finish_exec_io(&journal).await;
                return Err(error);
            }
        };
        let started_at_ms = now_ms();
        exec.state = ExecState::Running {
            process_id: process.id(),
            started_at_ms,
        };
        exec.checkpoint = None;
        if let Err(error) = self.execs.replace(&exec).await {
            return Err(self
                .rollback_unpublished_exec(exec.id.clone(), process, &journal, error)
                .await);
        }
        self.own_exec_output(exec.id, process, journal, io, started_at_ms, "process")
            .await;
        Ok(session)
    }

    /// Publishes a running exec's process and owns its output until the process exits.
    ///
    /// Both entrances to a live session end here -- one that started the command and one that
    /// reattached a restored domain member -- and they differ only in what the failure of the
    /// output owner is called, so `owner` names the launch rather than the code branching on it.
    async fn own_exec_output(
        self: &Arc<Self>,
        id: ExecId,
        process: Arc<dyn Running>,
        journal: JournalId,
        io: Arc<Io>,
        started_at_ms: u64,
        owner: &'static str,
    ) {
        let process_id = process.id();
        self.exec_live.lock().await.insert(id.clone(), Arc::clone(&process));
        let (output_complete, output_completion) = tokio::sync::watch::channel(false);
        self.exec_output_complete
            .lock()
            .await
            .insert(id.clone(), output_completion);
        self.failures.lock().await.remove(&journal);
        let service = Arc::clone(self);
        let owner_service = Arc::clone(&service);
        let owner_journal = journal.clone();
        // The generation this session is delivered through, kept so the completion path can close
        // it. `own` no longer does: the stream must not end before the exit status is recorded.
        let terminal = Arc::clone(&io);
        let handle = tokio::spawn(async move { owner_service.own(process, owner_journal, io, output_complete).await });
        let output_owner = Arc::new(super::OutputOwner {
            abort: handle.abort_handle(),
        });
        self.output_owners
            .lock()
            .await
            .insert(journal.clone(), Arc::clone(&output_owner));
        tokio::spawn(async move {
            let result = handle
                .await
                .map_err(|error| Error::Runtime(format!("{owner} output owner failed: {error}")))
                .and_then(std::convert::identity);
            service.retire_output_owner(&journal, &output_owner).await;
            service.finish_exec(id, process_id, started_at_ms, result).await;
            // `finish_exec` closes this generation on the paths that publish an exit; this covers
            // the ones that return early, so a drained session is never left waiting on a dead one.
            service.retire_io_generation(&journal, &terminal).await;
        });
    }

    /// Reattaches one restored domain member instead of relaunching its command.
    ///
    /// Restore is whole-image: `ckpt_restore_tree_body` forks every captured group out of the single
    /// `containers.start(...)` launch that owns the container's image, so a revived exec session is a
    /// forked child of the *container's* engine process rather than the subject of a launch of its own.
    /// Presenting one as a live session needs two things that a whole-image restore does not produce by
    /// itself, and this refuses unless it has both:
    ///
    /// * the process. [`Exec::guest_pid`](crate::Exec::guest_pid) records, before the freeze, the
    ///   container-namespace pid the image names the member by and the restore re-forks it under, and the
    ///   member announces itself under that number as it comes back. The capability the host holds it by
    ///   is the authenticated peer of the member's own channel, so it names the incarnation rather than
    ///   the number and can never be satisfied by whatever inherited the pid.
    /// * its I/O. `checkpoint/image.c` records guest fds 0..2 as `CKF_TTY`, which without a per-member
    ///   terminal could only be rebound to the container's single bridge -- and a session whose input goes
    ///   to a bridge shared with its container is not the session the user left. The terminal is created
    ///   for each sealed member at launch, before the restore starts, because the member asks for it from
    ///   inside its own descriptor restore.
    ///
    /// A member missing either keeps the refusal it already had, by name. What is never done is relaunch:
    /// [`Self::start_exec`] would run the session's original command a second time and present the result
    /// as the restored one, which restarts a `sleep`'s clock and runs a non-idempotent command twice.
    pub(crate) async fn reattach_exec(self: &Arc<Self>, id: &ExecId) -> Result<()> {
        let _guard = self.operations.lock().await;
        let mut exec = self.inspect_exec(id).await?;
        if !matches!(exec.state, ExecState::Created) || exec.checkpoint.is_none() {
            return Err(Error::InvalidExecState {
                id: exec.id,
                actual: exec.state,
                expected: "a sealed domain member awaiting restore",
            });
        }
        let launch = {
            let live = self.live.lock().await;
            live.get(&exec.container).map(|run| Arc::clone(&run.process))
        };
        // A container with no live launch and a record with no captured pid are one refusal: in
        // both, nothing in this process names the member the session is asking to be resumed on.
        let (Some(container), Some(guest_pid)) = (launch, exec.guest_pid) else {
            return Err(Error::ExecNotReattachable {
                id: exec.id,
                reason: MEMBER_HANDLE_GAP,
            });
        };
        let journal = JournalId::exec(exec.id.clone());
        let Some(process) = container.member_process(guest_pid) else {
            // Which half is missing decides which refusal is true, and both remain true refusals: a
            // member the restore never announced is unreachable, and one whose terminal this launch did
            // not create has nothing for a pane to attach to.
            let reason = if container.restored_member(guest_pid).is_some() {
                MEMBER_STDIO_GAP
            } else {
                MEMBER_HANDLE_GAP
            };
            return Err(Error::ExecNotReattachable { id: exec.id, reason });
        };
        let Some(io) = self.io.lock().await.get(&journal).cloned() else {
            return Err(Error::ExecNotReattachable {
                id: exec.id,
                reason: MEMBER_STDIO_GAP,
            });
        };
        let started_at_ms = now_ms();
        exec.state = ExecState::Running {
            process_id: process.id(),
            started_at_ms,
        };
        exec.checkpoint = None;
        self.execs.replace(&exec).await?;
        self.own_exec_output(exec.id, process, journal, io, started_at_ms, "restored member")
            .await;
        Ok(())
    }

    pub(crate) async fn attach_exec(
        self: &Arc<Self>,
        id: &ExecId,
        size: Option<crate::Size>,
    ) -> Result<crate::Session> {
        let _guard = self.operations.lock().await;
        let mut exec = self.running_exec(id).await?;
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
            self.resize_running_exec(&mut exec, size).await?;
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
        let armed = matches!(self.inspect_exec(&id).await, Ok(exec) if exec.checkpoint.is_some());
        if armed {
            // The engine's `wait()` reports a released member as a failure because its worker was
            // stopped by the coordinator rather than reaped normally. Recording that would make a
            // clean release indistinguishable from a crash, and would make `wait_exec` return a
            // runtime error for a member that is waiting to be restored.
            failure = None;
        }
        // A sealed domain member is released by the coordinator's committed capture and exits
        // cleanly afterwards. `checkpoint_locked` armed its token under the same `operations`
        // lock this function holds, so the token is already present here and the member keeps
        // the `Created` + armed-token shape a restore reads. Its own runtime bookkeeping must
        // still be retired below: a stale `exec_live` entry would quarantine the restore.
        if let Ok(mut exec) = self.inspect_exec(&id).await
            && !armed
        {
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
        // Retire only THIS process's bookkeeping. A released domain member's entries are already
        // retired by the capture that sealed it, and the restore that follows runs under the same
        // `operations` lock this function waits on -- so an unconditional removal here would delete
        // the RESTORED member's live entry and leave a running session with no runtime process.
        let owned = self
            .exec_live
            .lock()
            .await
            .get(&id)
            .is_some_and(|process| process.id() == owner);
        if owned {
            self.exec_live.lock().await.remove(&id);
            self.exec_output_complete.lock().await.remove(&id);
        }
        self.notify_exec_waiters(&id).await;
        self.finish_exec_io(&JournalId::exec(id)).await;
    }

    /// Wakes every waiter on an exec whose state or liveness has just changed.
    pub(super) async fn notify_exec_waiters(&self, id: &ExecId) {
        if let Some(waiters) = self.exec_waiters.lock().await.get(id) {
            waiters.notify_waiters();
        }
    }

    // Exec sessions are members of their container's freeze, not subjects of a
    // capture of their own: they hold the far ends of the container's sockets and
    // pipes, and a separate capture would stop one endpoint of a live connection
    // while the other still ran. There is deliberately no per-exec capture entry
    // point here, so no second checkpoint channel can be opened for a session.
    pub(crate) async fn resize_exec(&self, id: &ExecId, size: crate::Size) -> Result<()> {
        let _guard = self.operations.lock().await;
        let mut exec = self.running_exec(id).await?;
        self.resize_running_exec(&mut exec, size).await
    }

    /// Resizes a running exec's terminal and records the new size.
    ///
    /// The previous size is restored when the record cannot be written, so the process and the
    /// record a later attach reads never disagree about how wide the session is.
    async fn resize_running_exec(&self, exec: &mut Exec, size: crate::Size) -> Result<()> {
        let Some(previous) = exec.spec.process.console.terminal else {
            return Err(Error::NoTerminal(exec.id.to_string()));
        };
        let process = self.running_exec_process(&exec.id).await?;
        process.resize(size).await?;
        exec.spec.process.console.terminal = Some(size);
        if let Err(error) = self.execs.replace(exec).await {
            let _ = process.resize(previous).await;
            return Err(error);
        }
        Ok(())
    }

    /// Reads an exec that must be running, naming the state it is actually in when it is not.
    async fn running_exec(&self, id: &ExecId) -> Result<Exec> {
        let exec = self.inspect_exec(id).await?;
        if !matches!(exec.state, ExecState::Running { .. }) {
            return Err(Error::InvalidExecState {
                id: exec.id,
                actual: exec.state,
                expected: "running",
            });
        }
        Ok(exec)
    }

    /// The process a running exec's record names, which a completed reap has already retired.
    async fn running_exec_process(&self, id: &ExecId) -> Result<Arc<dyn Running>> {
        self.exec_live
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Runtime("running exec has no runtime process".into()))
    }

    pub(crate) async fn signal_exec(&self, id: &ExecId, signal: Signal) -> Result<()> {
        let _guard = self.operations.lock().await;
        let exec = self.running_exec(id).await?;
        self.running_exec_process(&exec.id).await?.signal(signal).await
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
        self.exec_output_complete.lock().await.remove(id);
        self.failures.lock().await.remove(&journal);
        self.exec_waiters.lock().await.remove(id);
        self.execs.remove(id).await
    }
}

/// Why a sealed member cannot be revived as a live exec session yet. Named once so the refusal
/// reads identically wherever it surfaces.
const MEMBER_HANDLE_GAP: &str = "the restored member is a forked child of the container's engine \
process; the runtime boundary exposes no handle for it and its launch-time stdio was rebound to \
the container's own bridge, so there is no live I/O to attach";

/// Why a member the restore DID announce still cannot be presented as a live session. The handle
/// exists and names the right process; only its I/O is missing.
const MEMBER_STDIO_GAP: &str = "the restored member is reachable, but its launch-time stdio was \
rebound to the container's own bridge at capture time, so it has no channel of its own for a \
terminal to attach to";
