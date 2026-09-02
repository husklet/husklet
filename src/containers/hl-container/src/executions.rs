use crate::{Error, Exec, ExecId, ExecSpec, ExecState, Result, Session, service::Service};
use std::sync::Arc;

/// Cheaply clonable service for additional processes owned by containers.
#[derive(Clone)]
pub struct Executions {
    service: Arc<Service>,
}

impl Executions {
    pub(crate) fn new(service: Arc<Service>) -> Self {
        Self { service }
    }

    /// Records a single-use execution against a running container.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, validation, or persistence failures.
    pub async fn create(&self, container: &str, spec: ExecSpec) -> Result<Exec> {
        self.service.create_exec(container, spec).await
    }

    /// Inspects a durable execution record.
    ///
    /// # Errors
    /// Returns not-found, corruption, or persistence failures.
    pub async fn inspect(&self, id: &ExecId) -> Result<Exec> {
        self.service.inspect_exec(id).await
    }

    /// Waits without polling until an execution reaches a terminal state.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, runtime, or persistence failures.
    pub async fn wait(&self, id: &ExecId) -> Result<crate::ExitStatus> {
        self.service.wait_exec(id).await
    }

    /// Lists durable execution records in creation order.
    ///
    /// # Errors
    /// Returns corruption or persistence failures.
    pub async fn list(&self) -> Result<Vec<Exec>> {
        self.service.list_execs().await
    }

    /// Returns the finite captured output emitted so far by one execution.
    pub async fn logs(&self, id: &ExecId) -> Result<crate::Logs> {
        self.service.exec_logs(id).await
    }

    /// Starts a created execution exactly once and returns its interactive session.
    ///
    /// Detached callers may drop the returned session after a successful start.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, runtime, or persistence failures.
    pub async fn start(&self, id: &ExecId) -> Result<Session> {
        self.service.start_exec(id, None, true).await
    }

    /// Starts a terminal execution at an exact initial size.
    ///
    /// The dimensions are applied before guest code begins executing.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, missing-terminal, runtime, or persistence failures.
    pub async fn start_at(&self, id: &ExecId, size: crate::Size) -> Result<Session> {
        self.service.start_exec(id, Some(size), true).await
    }

    /// Attaches to an execution which is already running.
    ///
    /// Unlike [`Self::start`], this never creates or restarts a process. It is
    /// intended for reconnecting to a checkpoint-restored interactive exec.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, missing-terminal, runtime, or persistence failures.
    pub async fn attach(&self, id: &ExecId, size: Option<crate::Size>) -> Result<Session> {
        self.service.attach_exec(id, size).await
    }

    /// Changes the size of a running execution's terminal.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, persistence, or missing-terminal failures.
    pub async fn resize(&self, id: &ExecId, size: crate::Size) -> Result<()> {
        self.service.resize_exec(id, size).await
    }

    /// Delivers a signal to one running execution without signaling its owning container.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, persistence, or runtime signaling failures.
    pub async fn signal(&self, id: &ExecId, signal: crate::Signal) -> Result<()> {
        self.service.signal_exec(id, signal).await
    }

    /// Removes one execution record and its captured output after it stops.
    ///
    /// # Errors
    /// Returns lookup, active-execution, or persistence cleanup failures.
    pub async fn remove(&self, id: &ExecId) -> Result<()> {
        self.service.remove_exec(id).await
    }

    /// Reattaches every execution whose durable checkpoint is ready to restore.
    ///
    /// A sealed member's process is revived by the container's own whole-image restore, so this
    /// never starts a process: relaunching the session's command would produce a second, fresh
    /// process alongside the restored one and report it as recovered. Where no handle for the
    /// restored member exists the record is reported as a failure, by name.
    ///
    /// Independent failures are returned together so startup can reattach every viable execution
    /// and report the complete degraded state instead of abandoning later records.
    ///
    /// # Errors
    /// Returns persistence failures encountered while listing execution records.
    pub async fn restore_checkpoints(&self) -> Result<Vec<(ExecId, Error)>> {
        let mut failures = Vec::new();
        for execution in self.list().await? {
            if execution.checkpoint.is_none() || !matches!(execution.state, ExecState::Created) {
                continue;
            }
            if let Err(error) = self.service.reattach_exec(&execution.id).await {
                failures.push((execution.id, error));
            }
        }
        Ok(failures)
    }
}
