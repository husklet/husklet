use super::Containers;
use crate::{Container, ContainerSpec, Result};

impl Containers {
    /// Creates and durably records a container without starting it.
    ///
    /// # Errors
    /// Returns validation, uniqueness, or persistence failures.
    pub async fn create(&self, spec: ContainerSpec) -> Result<Container> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "create");
        let container = self.service.create(spec).await?;
        hl_log::hl_info!(
            hl_log::tag::CONTAINER,
            "created id={} name={}",
            container.id,
            container.spec.name.as_deref().unwrap_or("-")
        );
        Ok(container)
    }

    /// Lists all containers in deterministic creation order.
    ///
    /// # Errors
    /// Returns persistence failures, including corrupt records.
    pub async fn list(&self) -> Result<Vec<Container>> {
        self.service.list().await
    }

    /// Resolves a container by full ID, unambiguous ID prefix, or name.
    ///
    /// # Errors
    /// Returns not-found, ambiguous-reference, or persistence failures.
    pub async fn inspect(&self, reference: &str) -> Result<Container> {
        self.service.inspect(reference).await
    }

    /// Sets one durable metadata label without changing runtime state.
    ///
    /// # Errors
    /// Returns lookup, validation, or persistence failures.
    pub async fn set_label(
        &self,
        reference: &str,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Container> {
        self.service.set_label(reference, name.into(), value.into()).await
    }

    /// Removes every container that is not currently running or paused.
    ///
    /// Each removal follows the normal ownership path, including logs and image-rootfs lease
    /// cleanup. A container that becomes active while pruning is skipped.
    ///
    /// # Errors
    /// Returns persistence or owned-resource cleanup failures.
    pub async fn prune(&self, selection: &crate::Prune) -> Result<Vec<Container>> {
        self.service.prune(selection).await
    }
}
