//! The host services the storybook offers an extension.
//!
//! Everything here is in memory. The storybook has no container runtime; it
//! only has to be a faithful counterpart, so the extension exercises the real
//! protocol rather than a shortcut.

use hl_extension::port::{
    ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
    TabSummary, TerminalSurface, WorkspaceFiles,
};
use hl_extension::{RelativePath, Services as Bundle};

/// A workspace that exists only for the storybook.
pub struct Workspace {
    containers: Vec<ContainerSummary>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    #[must_use]
    pub fn new() -> Self {
        Self {
            containers: super::containers(),
        }
    }

    /// The ports an extension's calls are dispatched to.
    #[must_use]
    pub fn services(&self) -> Bundle<'_> {
        Bundle {
            workspace: super::workspace(),
            containers: self,
            control: self,
            images: self,
            terminal: self,
            files: self,
        }
    }
}

impl ContainerInventory for Workspace {
    fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
        Ok(self.containers.clone())
    }

    fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
        self.containers
            .iter()
            .find(|container| container.id == id)
            .cloned()
            .ok_or_else(|| HostError::Absent(id.into()))
    }
}

/// The storybook grants no control, so these are never reached. They report a
/// conflict rather than pretending to succeed.
impl ContainerControl for Workspace {
    fn create(&self, _image: &str, _name: &str) -> Result<String, HostError> {
        Err(unavailable())
    }

    fn start(&self, _id: &str) -> Result<(), HostError> {
        Err(unavailable())
    }

    fn stop(&self, _id: &str) -> Result<(), HostError> {
        Err(unavailable())
    }

    fn remove(&self, _id: &str) -> Result<(), HostError> {
        Err(unavailable())
    }
}

impl ImageStore for Workspace {
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        Ok(Vec::new())
    }

    fn pull(&self, _reference: &str) -> Result<ImageSummary, HostError> {
        Err(unavailable())
    }
}

impl TerminalSurface for Workspace {
    fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
        Ok(Vec::new())
    }

    fn open_tab(&self, title: &str) -> Result<String, HostError> {
        Ok(format!("tab-{title}"))
    }

    fn split(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Err(unavailable())
    }

    fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
        Err(unavailable())
    }
}

impl WorkspaceFiles for Workspace {
    fn list(&self, _path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        Err(unavailable())
    }

    fn read(&self, _path: &RelativePath) -> Result<Vec<u8>, HostError> {
        Err(unavailable())
    }

    fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
        Err(unavailable())
    }
}

fn unavailable() -> HostError {
    HostError::Conflict("the storybook hosts no workspace".into())
}
