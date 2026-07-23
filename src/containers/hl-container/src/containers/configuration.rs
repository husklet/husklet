use super::Containers;
use crate::{Container, Result};

impl Containers {
    /// Atomically changes a container's unique name.
    ///
    /// # Errors
    /// Returns lookup, validation, uniqueness, or persistence failures.
    pub async fn rename(&self, reference: &str, name: impl Into<String>) -> Result<Container> {
        self.service.rename(reference, name.into()).await
    }

    /// Persists mutable launch limits and restart policy.
    ///
    /// Resource changes require an inactive container because the engine does not live-patch a
    /// running process. Restart policy changes apply to subsequent lifecycle decisions.
    ///
    /// # Errors
    /// Returns lookup, validation, persistence, or active-resource-change failures.
    pub async fn update(&self, reference: &str, update: crate::Update) -> Result<Container> {
        self.service.update(reference, update).await
    }
}
