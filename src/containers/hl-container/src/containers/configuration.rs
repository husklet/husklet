use super::Containers;
use crate::{Container, Result};

impl Containers {
    /// Discards an inactive container's process checkpoint while preserving its writable rootfs.
    ///
    /// # Errors
    /// Returns lookup, active-state, or persistence failures.
    pub async fn discard_checkpoint(&self, reference: &str) -> Result<Container> {
        self.service.discard_checkpoint(reference).await
    }

    /// Atomically changes a container's unique name.
    ///
    /// # Errors
    /// Returns lookup, validation, uniqueness, or persistence failures.
    pub async fn rename(&self, reference: &str, name: impl Into<String>) -> Result<Container> {
        self.service.rename(reference, name.into()).await
    }


    pub async fn rename_if_generation(
        &self,
        reference: &str,
        expected_id: &str,
        generation: u64,
        name: impl Into<String>,
    ) -> Result<Container> {
        let expected_id: crate::ContainerId = expected_id.parse::<crate::ContainerId>().map_err(|message| crate::Error::InvalidSpec(message.into()))?;
        self.service.rename_generation(reference, Some(&expected_id), Some(generation), name.into()).await
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
