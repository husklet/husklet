use crate::{service::Service, Exec, ExecId, ExecSpec, Result, Session};
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

    /// Starts a created execution exactly once and returns its interactive session.
    ///
    /// Detached callers may drop the returned session after a successful start.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, runtime, or persistence failures.
    pub async fn start(&self, id: &ExecId) -> Result<Session> {
        self.service.start_exec(id, None).await
    }

    /// Starts a terminal execution at an exact initial size.
    ///
    /// The dimensions are applied before guest code begins executing.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, missing-terminal, runtime, or persistence failures.
    pub async fn start_at(&self, id: &ExecId, size: crate::Size) -> Result<Session> {
        self.service.start_exec(id, Some(size)).await
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

    /// Verifies that every running execution has an armed checkpoint transport.
    ///
    /// # Errors
    /// Returns the first runtime or persistence failure.
    pub async fn require_checkpointable(&self) -> Result<()> {
        self.service.checkpointable_execs().await.map(|_| ())
    }

    /// Checkpoints every running execution without checkpointing its container's initial process.
    ///
    /// # Errors
    /// Returns the first capture, runtime, or persistence failure.
    pub async fn checkpoint_all(&self, timeout: std::time::Duration) -> Result<()> {
        self.service.checkpoint_execs(timeout).await
    }
}
