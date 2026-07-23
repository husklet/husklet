use super::Containers;
use crate::Result;
use std::sync::Arc;

impl Containers {
    /// Returns the additional-process service sharing this container ownership graph.
    #[must_use]
    pub fn executions(&self) -> crate::Executions {
        crate::Executions::new(Arc::clone(&self.service))
    }

    /// Returns captured stdout and stderr emitted so far by the initial process.
    ///
    /// After [`Self::wait`] returns, both streams are fully drained and durable.
    ///
    /// # Errors
    /// Returns lookup or log-storage failures.
    pub async fn logs(&self, reference: &str) -> Result<crate::Logs> {
        self.service.logs(reference).await
    }

    /// Attaches a cursor to future process output. Calling this before start captures byte zero.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, or persistence failures.
    pub async fn attach(&self, reference: &str) -> Result<crate::Session> {
        self.service.attach(reference).await
    }

    /// Replays all durable output, then follows the process until its output journal closes.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, or persistence failures.
    pub async fn follow(&self, reference: &str) -> Result<crate::Session> {
        self.service.follow(reference).await
    }
}
