//! Adapters that make the extension protocol's declared ports real.
//!
//! `hl-extension` owns the ports and deliberately depends on no container
//! stack; this module is the composition-root half of that split. It calls
//! `hl-client` for container and image effects and the workspace storage
//! directory for files.
//!
//! The ports are synchronous because the extension session dispatches on a
//! blocking thread. The container client is asynchronous, so every adapter
//! shares one owned [`Bridge`] that holds a small runtime and blocks on it —
//! the same shape `runtime::execution` uses for terminal panes, and for the
//! same reason: no global runtime, and no async in the port traits.
//!
//! The terminal port is not implemented here. It belongs to the GUI thread
//! that owns the surface.

mod control;
mod conversation;
mod files;
mod host;
mod image;
mod inventory;
mod listener;
mod resource;
mod registration;
mod roster;
mod sidecar;
mod state;
mod terminal;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use hl_extension::port::{ContainerControl, ContainerInventory, HostError, ImageStore, NetworkStore, VolumeStore, WorkspaceFiles};

pub use control::ContainerLifecycle;
pub use conversation::{Conversation, Interface, Queue};
pub use files::WorkspaceDirectory;
pub use host::{Audience, Host, Order, Overrun, Plan, Report, Standing, Supply};
pub use image::ImageLibrary;
pub use inventory::ContainerCatalog;
pub use listener::Listener;
pub use resource::Resources;
pub use registration::{Acquisition, Candidate};
pub use roster::{described, Entry, Refusal, Roster};
pub use sidecar::{Image, Outcome, Sidecar, SidecarSpec};
pub use state::{Fault, Records};
pub use terminal::{Answer, Errand, Errands, Relay, Request};

use crate::config::WorkspaceConfig;

/// The one place a blocking port method meets the asynchronous container client.
///
/// Owns its runtime rather than reaching for an ambient one, so an extension
/// host can be constructed, dropped, and constructed again without leaving a
/// process-wide executor behind.
pub struct Bridge {
    runtime: tokio::runtime::Runtime,
    client: hl_client::Client,
}

impl Bridge {
    /// Two workers match the terminal launcher: enough for a request and the
    /// connection driver that carries it, and no thread pool sized for work
    /// this host does not do.
    fn new(socket: PathBuf) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let client = hl_client::Client::unix(socket).map_err(io::Error::other)?;
        Ok(Self { runtime, client })
    }

    pub(super) fn client(&self) -> &hl_client::Client {
        &self.client
    }

    /// Runs one client call to completion on the owned runtime.
    ///
    /// Callers are blocking port methods, never runtime threads, so this cannot
    /// be reached from inside `runtime`.
    pub(super) fn wait<F: std::future::Future>(&self, work: F) -> F::Output {
        self.runtime.block_on(work)
    }
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Bridge").finish_non_exhaustive()
    }
}

/// Translates a container client error into the protocol's host error.
///
/// A missing container or image must read as [`HostError::Absent`] rather than
/// a breakage, because the caller distinguishes "there is no such thing" from
/// "the host is broken", and only the second is worth reporting as a fault.
pub(super) fn failure(error: &hl_client::Error) -> HostError {
    let hl_client::Error::Docker { status, message } = error else {
        return HostError::Failed(error.to_string());
    };
    status_failure(status.as_u16(), message)
}

/// The status half of [`failure`], separated so the mapping is testable without
/// constructing an HTTP response.
fn status_failure(status: u16, message: &str) -> HostError {
    match status {
        404 => HostError::Absent(message.to_owned()),
        304 | 409 => HostError::Conflict(message.to_owned()),
        501 => HostError::Unsupported(message.to_owned()),
        _ => HostError::Failed(message.to_owned()),
    }
}

/// The host services one workspace offers its extensions.
///
/// Owns the bridge every port shares and hands out borrowed ports, so the
/// session's `Services` bundle can be assembled without any port owning a
/// runtime of its own.
pub struct Extensions {
    containers: ContainerCatalog,
    control: ContainerLifecycle,
    images: ImageLibrary,
    resources: Resources,
    files: WorkspaceDirectory,
}

impl Extensions {
    /// Starts the workspace's execution domain if needed and binds the ports to it.
    ///
    /// # Errors
    /// Returns the failure to reach the workspace domain, build a runtime, or
    /// prepare the workspace storage directory.
    pub fn open(workspace: &WorkspaceConfig) -> io::Result<Self> {
        let socket = crate::runtime::domain::Domain::new(workspace).ensure(workspace)?;
        let bridge = Arc::new(Bridge::new(socket)?);
        let root = workspace.storage_dir(&crate::paths::hl_root()).join("files");
        Ok(Self {
            containers: ContainerCatalog::new(Arc::clone(&bridge)),
            control: ContainerLifecycle::new(Arc::clone(&bridge)),
            images: ImageLibrary::new(Arc::clone(&bridge)),
            resources: Resources::new(bridge),
            files: WorkspaceDirectory::new(root)?,
        })
    }

    /// The container reading port.
    #[must_use]
    pub fn containers(&self) -> &dyn ContainerInventory {
        &self.containers
    }

    /// The container control port.
    #[must_use]
    pub fn control(&self) -> &dyn ContainerControl {
        &self.control
    }

    /// The image port.
    #[must_use]
    pub fn images(&self) -> &dyn ImageStore {
        &self.images
    }

    #[must_use]
    pub fn volumes(&self) -> &dyn VolumeStore { &self.resources }

    #[must_use]
    pub fn networks(&self) -> &dyn NetworkStore { &self.resources }

    /// The workspace file port.
    #[must_use]
    pub fn files(&self) -> &dyn WorkspaceFiles {
        &self.files
    }
}

#[cfg(test)]
mod tests {
    use super::{failure, status_failure};
    use hl_extension::port::HostError;

    #[test]
    fn a_missing_container_is_absent_rather_than_a_failure() {
        assert_eq!(
            status_failure(404, "no such container: ghost"),
            HostError::Absent("no such container: ghost".to_owned())
        );
    }

    #[test]
    fn a_state_refusal_is_a_conflict_and_anything_else_is_a_failure() {
        assert_eq!(
            status_failure(409, "container is running"),
            HostError::Conflict("container is running".to_owned())
        );
        assert_eq!(
            status_failure(304, "already started"),
            HostError::Conflict("already started".to_owned())
        );
        assert!(matches!(status_failure(500, "boom"), HostError::Failed(_)));
        assert!(matches!(
            status_failure(501, "process sampling is unavailable"),
            HostError::Unsupported(_)
        ));
    }

    #[test]
    fn a_transport_failure_carries_no_absence() {
        assert!(matches!(failure(&hl_client::Error::Timeout), HostError::Failed(_)));
    }
}
