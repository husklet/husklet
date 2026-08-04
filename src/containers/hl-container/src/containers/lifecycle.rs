use super::Containers;
use crate::{Container, ExitStatus, Result, Signal, WaitCondition};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleAction {
    Create,
    Start,
    Die,
    Restart,
    Destroy,
    Oom,
    HealthStatus,
}

#[derive(Clone, Debug)]
pub struct LifecycleEvent {
    pub action: LifecycleAction,
    pub container: Container,
}

pub trait LifecycleEvents: Send + Sync + 'static {
    fn emit(&self, event: LifecycleEvent);
}

impl Containers {
    pub fn observe(&self, events: Arc<dyn LifecycleEvents>) {
        self.service.observe(events);
    }
    /// Starts the initial process. Exactly one task owns its runtime handle and exit transition.
    ///
    /// # Errors
    /// Returns lookup, state, runtime-launch, or persistence failures.
    pub async fn start(&self, reference: &str) -> Result<()> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "start");
        self.service.start(reference).await?;
        hl_log::hl_info!(hl_log::tag::CONTAINER, "started reference={reference}");
        Ok(())
    }

    /// Waits for and returns the durable terminal status. Multiple concurrent waiters are supported.
    ///
    /// # Errors
    /// Returns lookup, invalid-state, runtime, or persistence failures.
    pub async fn wait(&self, reference: &str) -> Result<ExitStatus> {
        self.service
            .wait(reference, WaitCondition::NotRunning)
            .await?
            .ok_or_else(|| crate::Error::NotFound(reference.into()))
    }

    /// Waits for an explicit lifecycle condition.
    ///
    /// `Removed` returns `None`; `NotRunning` and `NextExit` return the process result.
    ///
    /// # Errors
    /// Returns lookup, invalid-state, runtime, or persistence failures.
    pub async fn wait_for(&self, reference: &str, condition: WaitCondition) -> Result<Option<ExitStatus>> {
        self.service.wait(reference, condition).await
    }

    /// Delivers a signal to a running container without fabricating a state transition.
    ///
    /// # Errors
    /// Returns lookup, invalid-state, or host process-control failures.
    pub async fn signal(&self, reference: &str, signal: Signal) -> Result<()> {
        self.service.signal(reference, signal).await
    }

    /// Changes the size of a running container's terminal and preserves it for restarts.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, persistence, or missing-terminal failures.
    pub async fn resize(&self, reference: &str, size: crate::Size) -> Result<()> {
        self.service.resize(reference, size).await
    }

    /// Suspends every thread in the container's initial process.
    ///
    /// # Errors
    /// Returns lookup, state, process-control, or persistence failures.
    pub async fn pause(&self, reference: &str) -> Result<()> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "pause");
        self.service.pause(reference).await?;
        hl_log::hl_info!(hl_log::tag::CONTAINER, "paused reference={reference}");
        Ok(())
    }

    /// Resumes a container previously suspended by [`Self::pause`].
    ///
    /// # Errors
    /// Returns lookup, state, process-control, or persistence failures.
    pub async fn unpause(&self, reference: &str) -> Result<()> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "unpause");
        self.service.unpause(reference).await?;
        hl_log::hl_info!(hl_log::tag::CONTAINER, "resumed reference={reference}");
        Ok(())
    }

    /// Writes the complete process tree to durable native checkpoint storage.
    ///
    /// Success means the engine published its final manifest and intentionally stopped. Calling
    /// [`Self::start`] on the same container restores that process tree and arms the next checkpoint.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, resource-compatibility, timeout, runtime, or persistence failures.
    pub async fn checkpoint(&self, reference: &str, timeout: std::time::Duration) -> Result<crate::Checkpoint> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "checkpoint");
        let checkpoint = self.service.checkpoint(reference, timeout).await?;
        hl_log::hl_info!(
            hl_log::tag::CONTAINER,
            "checkpointed reference={reference} namespace={}",
            checkpoint.namespace
        );
        Ok(checkpoint)
    }

    /// Requests termination, waits up to `timeout`, then force-kills and reaps the process.
    ///
    /// # Errors
    /// Returns lookup, invalid-state, runtime, or persistence failures.
    pub async fn stop(&self, reference: &str, timeout: std::time::Duration) -> Result<ExitStatus> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "stop");
        let status = self.service.stop(reference, timeout).await?;
        hl_log::hl_info!(
            hl_log::tag::CONTAINER,
            "stopped reference={reference} status={status:?}"
        );
        Ok(status)
    }

    /// Stops every active container while preserving the first failure.
    ///
    /// Shutdown continues after an individual failure so one broken process cannot leave unrelated
    /// containers running. Callers must stop accepting new lifecycle operations before invoking this.
    ///
    /// # Errors
    /// Returns the first lookup, signaling, waiting, or persistence failure after attempting every active
    /// container.
    pub async fn shutdown(&self, timeout: std::time::Duration) -> Result<()> {
        let active = self
            .list()
            .await?
            .into_iter()
            .filter(|container| container.state.is_active())
            .collect::<Vec<_>>();
        let mut failure = None;
        for container in active {
            let result = self.stop(container.id.as_str(), timeout).await;
            let error = match result {
                Ok(_) | Err(crate::Error::NotFound(_)) => None,
                Err(crate::Error::InvalidState { actual, .. }) if !actual.is_active() => None,
                Err(error) => Some(error),
            };
            if failure.is_none() {
                failure = error;
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Checkpoints every running container so the complete execution domain can continue later.
    ///
    /// Paused containers are resumed only long enough for the engine to capture them. Processing continues
    /// after an individual failure so callers receive the first error without abandoning unrelated
    /// containers.
    ///
    /// # Errors
    /// Returns the first resume, checkpoint, lifecycle, or persistence failure.
    pub async fn checkpoint_all(&self, timeout: std::time::Duration) -> Result<()> {
        self.service.checkpoint_execs(timeout).await?;
        let mut failure = None;
        let active = self.list().await?;
        for container in active {
            let result = match container.state {
                crate::ContainerState::Running { .. } => {
                    self.checkpoint(container.id.as_str(), timeout).await.map(|_| ())
                }
                crate::ContainerState::Paused { .. } => match self.unpause(container.id.as_str()).await {
                    Ok(()) => self.checkpoint(container.id.as_str(), timeout).await.map(|_| ()),
                    Err(error) => Err(error),
                },
                crate::ContainerState::Restarting { .. } => self.signal(container.id.as_str(), Signal::Terminate).await,
                crate::ContainerState::Created | crate::ContainerState::Exited { .. } => Ok(()),
            };
            let error = match result {
                Ok(()) | Err(crate::Error::NotFound(_)) => None,
                Err(error) => Some(error),
            };
            if failure.is_none() {
                failure = error;
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Verifies that every active attached execution has a checkpoint transport before shutdown starts.
    ///
    /// # Errors
    /// Returns a runtime error naming the first active process that cannot be checkpointed.
    pub async fn require_checkpointable(&self) -> Result<()> {
        self.service.checkpointable_execs().await.map(|_| ())
    }

    /// Removes a created or exited container. Running containers are rejected.
    ///
    /// # Errors
    /// Returns lookup, invalid-state, or persistence failures.
    pub async fn remove(&self, reference: &str) -> Result<Container> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "remove");
        let container = self.service.remove(reference, false, false, None).await?;
        hl_log::hl_info!(hl_log::tag::CONTAINER, "removed id={}", container.id);
        Ok(container)
    }

    /// Force-stops a running process, waits for durable exit, then removes its metadata.
    ///
    /// # Errors
    /// Returns lookup, runtime, persistence, or rootfs-release failures.
    pub async fn remove_force(&self, reference: &str) -> Result<Container> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "remove_force");
        let container = self.service.remove(reference, true, false, None).await?;
        hl_log::hl_info!(hl_log::tag::CONTAINER, "force removed id={}", container.id);
        Ok(container)
    }

    /// Removes a container and its container-owned anonymous volumes.
    ///
    /// Named volumes and anonymous volumes still referenced by another container are preserved.
    ///
    /// # Errors
    /// Returns lookup, state, runtime, persistence, or owned-resource cleanup failures.
    pub async fn remove_volumes(&self, reference: &str, force: bool) -> Result<Container> {
        self.service.remove(reference, force, true, None).await
    }
}
