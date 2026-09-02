use http::Method;

use crate::Result;
use crate::model::{Volume, VolumeCreate, VolumeList, VolumePrune};
use crate::transport::Transport;
use crate::uri::Component;

/// Typed local-volume operations.
#[derive(Clone, Copy, Debug)]
pub struct Volumes<'a> {
    transport: &'a Transport,
}

impl<'a> Volumes<'a> {
    pub(crate) fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    /// List every local volume visible to the daemon.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn list(&self) -> Result<VolumeList> {
        self.transport.get_json("/volumes").await
    }

    /// Create a named or anonymous local volume.
    ///
    /// An empty [`VolumeCreate::name`] asks the daemon to allocate a unique name.
    ///
    /// # Errors
    /// Returns transport, validation, driver, storage, or response-decoding failures.
    pub async fn create(&self, request: &VolumeCreate) -> Result<Volume> {
        self.transport
            .json(Method::POST, "/volumes/create", Some(request))
            .await
    }

    /// Inspect one local volume by name.
    ///
    /// # Errors
    /// Returns transport, not-found, Docker API, or response-decoding failures.
    pub async fn inspect(&self, name: &str) -> Result<Volume> {
        self.transport
            .get_json(&format!("/volumes/{}", Component::opaque(name)))
            .await
    }

    /// Remove one local volume.
    ///
    /// Docker may still reject a referenced volume when `force` is true.
    ///
    /// # Errors
    /// Returns transport, not-found, in-use, storage, or Docker API failures.
    pub async fn remove(&self, name: &str, force: bool) -> Result<()> {
        self.transport
            .empty(
                Method::DELETE,
                &format!("/volumes/{}?force={force}", Component::opaque(name)),
            )
            .await
    }

    /// Remove only if the authoritative generation still matches inspection.
    pub async fn remove_if_generation(&self, name: &str, generation: &str) -> Result<()> {
        self.transport
            .empty(
                Method::DELETE,
                &format!("/volumes/{}?generation={}", Component::opaque(name), Component::opaque(generation)),
            )
            .await
    }

    /// Remove every unused local volume.
    ///
    /// # Errors
    /// Returns transport, storage, Docker API, or response-decoding failures.
    pub async fn prune(&self) -> Result<VolumePrune> {
        self.transport
            .json::<(), VolumePrune>(Method::POST, "/volumes/prune", None)
            .await
    }
}
