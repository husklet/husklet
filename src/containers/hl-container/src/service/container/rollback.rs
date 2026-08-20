//! Undoing a launch whose process outlived the record that was supposed to name it.
//!
//! Both `Runtime::start` entrances -- a container and an exec session -- publish their state
//! *after* the process exists, so both have a window in which a live guest process has no record
//! and nothing else will ever reap it. The rollback closes that window: it kills the process,
//! waits a bounded time for the reap, and when the reap outlives that window it quarantines the
//! identity and settles the outcome detached. The two halves live together because they share the
//! bound, the quarantine discipline, and the failure vocabulary; splitting them left the bound
//! duplicated verbatim in two files.

use super::{Arc, Error, ExecId, ExitStatus, Io, JournalId, Result, Running, Service, Signal};

impl Service {
    /// Rolls back a container launch whose state could not be persisted.
    pub(super) async fn rollback_unpublished_launch(
        self: &Arc<Self>,
        id: crate::ContainerId,
        process: Arc<dyn Running>,
        journal: &JournalId,
        io: &Arc<Io>,
        publication: Error,
    ) -> Error {
        let mut cleanup = Vec::new();
        if let Err(error) = process.signal(Signal::KILL).await {
            cleanup.push(format!("kill failed: {error}"));
        }
        let mut wait = tokio::spawn(Arc::clone(&process).wait());
        match tokio::time::timeout(unpublished_reap_timeout(), &mut wait).await {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(error))) => {
                let failure = format!("unpublished process reap failed: {error}");
                self.poison_launch_cleanup(id.clone(), failure.clone()).await;
                cleanup.push(failure);
            }
            Ok(Err(error)) => {
                let failure = format!("unpublished reap task failed: {error}");
                self.poison_launch_cleanup(id.clone(), failure.clone()).await;
                cleanup.push(failure);
            }
            Err(_) => {
                cleanup.push(format!("reap timed out after {:?}", unpublished_reap_timeout()));
                let cleanup_task = tokio::spawn(settle_late_reap(Arc::downgrade(self), id.clone(), wait));
                self.launch_cleanups
                    .lock()
                    .await
                    .insert(id, cleanup_task.abort_handle());
            }
        }
        self.retire_io_generation(journal, io).await;
        if cleanup.is_empty() {
            publication
        } else {
            Error::Runtime(format!(
                "failed to persist start ({publication}); process cleanup also failed ({})",
                cleanup.join("; ")
            ))
        }
    }

    async fn poison_launch_cleanup(&self, id: crate::ContainerId, failure: String) {
        self.launch_cleanup_failures.lock().await.insert(id, failure);
    }

    /// Rolls back an exec session whose state could not be persisted.
    ///
    /// The exec half carries more than the container half because an exec identity survives its
    /// process: a quarantined exec keeps its `exec_live` entry so a second start is refused by
    /// name rather than launching a second process against the same record.
    pub(super) async fn rollback_unpublished_exec(
        self: &Arc<Self>,
        id: ExecId,
        process: Arc<dyn Running>,
        journal: &JournalId,
        publication: Error,
    ) -> Error {
        let mut cleanup = Vec::new();
        if let Err(error) = process.signal(Signal::KILL).await {
            cleanup.push(format!("kill failed: {error}"));
        }
        let mut wait = tokio::spawn(Arc::clone(&process).wait());
        let mut terminal_failure = None;
        let reaped = match tokio::time::timeout(unpublished_reap_timeout(), &mut wait).await {
            Ok(Ok(Ok(_))) => true,
            Ok(Ok(Err(error))) => {
                cleanup.push(format!("reap failed: {error}"));
                terminal_failure = Some(error.to_string());
                true
            }
            Ok(Err(error)) => {
                cleanup.push(format!("reap task failed: {error}"));
                false
            }
            Err(_) => {
                cleanup.push(format!("reap timed out after {:?}", unpublished_reap_timeout()));
                self.exec_live.lock().await.insert(id.clone(), Arc::clone(&process));
                let cleanup_task = tokio::spawn(settle_late_exec_reap(
                    Arc::downgrade(self),
                    id.clone(),
                    journal.clone(),
                    process,
                    wait,
                ));
                self.exec_cleanups.lock().await.insert(id, cleanup_task.abort_handle());
                return Error::Runtime(format!(
                    "exec state publication failed: {publication}; rollback cleanup failed: {}",
                    cleanup.join("; ")
                ));
            }
        };
        if reaped {
            if let Some(error) = terminal_failure {
                self.record_exec_failure(journal, format!("unpublished exec cleanup failed: {error}"))
                    .await;
                self.notify_exec_waiters(&id).await;
            }
            self.finish_exec_io(journal).await;
        } else {
            self.exec_live.lock().await.insert(id.clone(), Arc::clone(&process));
            let failure = format!("unpublished exec cleanup is quarantined: {}", cleanup.join("; "));
            self.record_exec_failure(journal, failure.clone()).await;
            self.exec_cleanup_failures.lock().await.insert(id.clone(), failure);
            self.notify_exec_waiters(&id).await;
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

    /// Waits for every quarantined exec reap to settle, refusing at once if one is poisoned.
    pub(crate) async fn await_exec_cleanups(&self, timeout: std::time::Duration) -> Result<()> {
        self.exec_cleanups.lock().await.retain(|_, task| !task.is_finished());
        if let Some((id, error)) = self.exec_cleanup_failures.lock().await.iter().next() {
            return Err(Error::Runtime(format!("exec {id} cleanup is poisoned: {error}")));
        }
        if self.exec_cleanups.lock().await.is_empty() {
            return Ok(());
        }
        tokio::time::timeout(timeout, async {
            loop {
                self.exec_cleanups.lock().await.retain(|_, task| !task.is_finished());
                if self.exec_cleanups.lock().await.is_empty() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| Error::Runtime("timed out waiting for quarantined exec cleanup".into()))
    }

    async fn record_exec_failure(&self, journal: &JournalId, failure: String) {
        self.failures.lock().await.insert(journal.clone(), failure);
    }

    /// Retires the runtime bookkeeping of a quarantined exec whose process has finally been reaped.
    async fn retire_quarantined_exec(&self, id: &ExecId, journal: &JournalId) {
        self.exec_live.lock().await.remove(id);
        self.finish_exec_io(journal).await;
        self.notify_exec_waiters(id).await;
    }
}

/// Records the outcome of an unpublished container process whose reap outlived the rollback that
/// started it.
///
/// The rollback could wait no longer, so this runs detached and reports through the service's
/// poison ledger instead. A service that has been dropped has nobody left to report to.
async fn settle_late_reap<T>(
    service: std::sync::Weak<Service>,
    id: crate::ContainerId,
    wait: tokio::task::JoinHandle<Result<T>>,
) {
    let result = wait.await;
    let Some(service) = service.upgrade() else {
        return;
    };
    let _guard = service.operations.lock().await;
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            service
                .poison_launch_cleanup(id.clone(), format!("unpublished process reap failed: {error}"))
                .await;
        }
        Err(error) => {
            service
                .poison_launch_cleanup(id.clone(), format!("unpublished reap task failed: {error}"))
                .await;
        }
    }
    service.launch_cleanups.lock().await.remove(&id);
}

/// Records the outcome of a quarantined exec process whose reap outlived the rollback that started
/// it, and releases the identity the quarantine was holding.
///
/// The `exec_live` entry is retired only when it still names *this* process: a start that raced the
/// quarantine would have been refused, but a restore under the same identity must not have its live
/// process deleted by a reap belonging to the incarnation before it.
async fn settle_late_exec_reap(
    service: std::sync::Weak<Service>,
    id: ExecId,
    journal: JournalId,
    process: Arc<dyn Running>,
    wait: tokio::task::JoinHandle<Result<ExitStatus>>,
) {
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
                service.retire_quarantined_exec(&id, &journal).await;
            }
        }
        Ok(Err(error)) => {
            service.exec_live.lock().await.remove(&id);
            service
                .record_exec_failure(&journal, format!("unpublished exec cleanup failed: {error}"))
                .await;
            service.finish_exec_io(&journal).await;
            service.notify_exec_waiters(&id).await;
        }
        Err(error) => {
            let failure = format!("quarantined exec reap task failed: {error}");
            hl_log::hl_error!(hl_log::tag::CONTAINER, "{} id={}", failure, id);
            service.record_exec_failure(&journal, failure.clone()).await;
            service.exec_cleanup_failures.lock().await.insert(id.clone(), failure);
            service.notify_exec_waiters(&id).await;
            service.exec_cleanups.lock().await.remove(&id);
            return;
        }
    }
    service.exec_cleanups.lock().await.remove(&id);
}

/// How long a rollback waits for the process it killed before quarantining the reap.
///
/// The test value is short because every fixture that exercises the quarantine has to reach it
/// through this wait; the production value is the one a real reap needs.
#[cfg(test)]
fn unpublished_reap_timeout() -> std::time::Duration {
    std::time::Duration::from_millis(25)
}

#[cfg(not(test))]
fn unpublished_reap_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(5)
}
