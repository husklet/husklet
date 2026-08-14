//! Changing container state on behalf of an extension.

use std::sync::Arc;

use hl_client::model::CreateContainer;
use hl_extension::port::{ContainerControl, HostError};

use super::{failure, Bridge};

/// How long a stop waits for the initial process before the daemon forces it.
const STOP_SECONDS: u64 = 10;

/// The container control port over the workspace's container daemon.
///
/// Reaching this port is reaching code execution inside the workspace, which is
/// why the protocol gates it behind its own capability.
pub struct ContainerLifecycle {
    bridge: Arc<Bridge>,
}

impl ContainerLifecycle {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self {
        Self { bridge }
    }
}

impl ContainerControl for ContainerLifecycle {
    /// Creates a container from an image already present locally.
    ///
    /// Pulling stays in the image port so a control grant alone cannot reach
    /// the network.
    ///
    /// # Errors
    /// Returns `HostError::Absent` for an unknown image, `HostError::Conflict`
    /// for a name already taken, and a failure otherwise.
    fn create(&self, image: &str, name: &str) -> Result<String, HostError> {
        let request = CreateContainer {
            image: image.to_owned(),
            ..CreateContainer::default()
        };
        let client = self.bridge.client();
        let created = self
            .bridge
            .wait(client.containers().create(&request, Some(name)))
            .map_err(|error| failure(&error))?;
        Ok(created.id)
    }

    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    fn start(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().start(id))
            .map_err(|error| failure(&error))
    }

    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    fn stop(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().stop(id, Some(STOP_SECONDS)))
            .map_err(|error| failure(&error))
    }

    /// Removes a container without forcing it.
    ///
    /// A running container is reported as a conflict rather than killed: an
    /// extension that means to stop it can say so.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no such container exists and
    /// `HostError::Conflict` when it is still running.
    fn remove(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().remove(id, false, false))
            .map_err(|error| failure(&error))
    }
}
