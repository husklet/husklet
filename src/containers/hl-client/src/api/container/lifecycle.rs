//! Container lifecycle transitions: start, stop, signal, and removal.

use http::Method;

use crate::Result;
use crate::model::Wait;
use crate::uri::Component;

use super::{Containers, WaitCondition};
use crate::api::Size;

impl Containers<'_> {
    /// Read metadata for a path in the container filesystem.
    ///
    /// # Errors
    /// Start the container's initial process.
    ///
    /// # Errors
    /// Returns transport, state, runtime, or daemon failures.
    pub async fn start(&self, id: &str) -> Result<()> {
        self.transport
            .empty(Method::POST, &format!("/containers/{}/start", Component::opaque(id)))
            .await
    }

    /// Resize a running container terminal.
    ///
    /// # Errors
    /// Returns transport, lookup, lifecycle, or missing-terminal failures.
    pub async fn resize(&self, id: &str, size: Size) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/resize?{}", Component::opaque(id), size.query()),
            )
            .await
    }

    /// Gracefully stop a running container, then force it after the optional timeout.
    ///
    /// # Errors
    /// Returns transport, state, runtime, or daemon failures.
    pub async fn stop(&self, id: &str, timeout_seconds: Option<u64>) -> Result<()> {
        let query = timeout_seconds.map_or_else(String::new, |seconds| format!("?t={seconds}"));
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/stop{query}", Component::opaque(id)),
            )
            .await
    }

    /// Stop and start a running container.
    ///
    /// # Errors
    /// Returns transport, state, runtime, or daemon failures.
    pub async fn restart(&self, id: &str, timeout_seconds: Option<u64>) -> Result<()> {
        let query = timeout_seconds.map_or_else(String::new, |seconds| format!("?t={seconds}"));
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/restart{query}", Component::opaque(id)),
            )
            .await
    }

    /// Deliver a Linux signal name or number to a running container.
    ///
    /// # Errors
    /// Returns transport, signal-validation, state, runtime, or daemon failures.
    pub async fn kill(&self, id: &str, signal: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/containers/{}/kill?signal={}",
                    Component::opaque(id),
                    Component::opaque(signal)
                ),
            )
            .await
    }

    /// Atomically assign a new unique container name.
    ///
    /// # Errors
    /// Returns transport, validation, conflict, persistence, or daemon failures.
    pub async fn rename(&self, id: &str, name: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/containers/{}/rename?name={}",
                    Component::opaque(id),
                    Component::opaque(name)
                ),
            )
            .await
    }

    /// Suspend the container's initial process.
    ///
    /// # Errors
    /// Returns transport, state, process-control, or daemon failures.
    pub async fn pause(&self, id: &str) -> Result<()> {
        self.transport
            .empty(Method::POST, &format!("/containers/{}/pause", Component::opaque(id)))
            .await
    }

    /// Resume a paused container's initial process.
    ///
    /// # Errors
    /// Returns transport, state, process-control, or daemon failures.
    pub async fn unpause(&self, id: &str) -> Result<()> {
        self.transport
            .empty(Method::POST, &format!("/containers/{}/unpause", Component::opaque(id)))
            .await
    }

    /// Persist the complete container process tree and stop the live engine.
    ///
    /// A subsequent [`Self::start`] restores the container from the published checkpoint.
    ///
    /// # Errors
    /// Returns transport, state, timeout, resource-compatibility, or daemon failures.
    pub async fn checkpoint(&self, id: &str, timeout: std::time::Duration) -> Result<()> {
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/containers/{}/checkpoint?timeout_ms={timeout_ms}",
                    Component::opaque(id)
                ),
            )
            .await
    }
    /// Wait for the container's initial process to stop.
    ///
    /// # Errors
    /// Returns transport, state, runtime, decoding, or daemon failures.
    pub async fn wait(&self, id: &str) -> Result<Wait> {
        self.wait_for(id, WaitCondition::NotRunning).await
    }

    /// Wait for a specific Docker lifecycle condition.
    ///
    /// # Errors
    /// Returns transport, state, runtime, decoding, or daemon failures.
    pub async fn wait_for(&self, id: &str, condition: WaitCondition) -> Result<Wait> {
        self.transport
            .json::<(), Wait>(
                Method::POST,
                &format!(
                    "/containers/{}/wait?condition={}",
                    Component::opaque(id),
                    condition.as_str()
                ),
                None,
            )
            .await
    }
    /// Remove a container and optionally force-stop it first.
    ///
    /// # Errors
    /// Returns transport, state, runtime, persistence, or daemon failures.
    pub async fn remove(&self, id: &str, force: bool, volumes: bool) -> Result<()> {
        self.transport
            .empty(
                Method::DELETE,
                &format!("/containers/{}?force={force}&v={volumes}", Component::opaque(id)),
            )
            .await
    }
}
