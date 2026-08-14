//! The real supply: one workspace, its records, and its container daemon.
//!
//! Everything in this file needs a running workspace, which is why it is the
//! one part of the host the suite cannot drive. It is kept apart from the
//! orchestration for exactly that reason: what can be tested and what cannot
//! are not mixed in one file.

use std::path::PathBuf;
use std::sync::Arc;

use hl_ws_extension::port::{Division, HostError, TabSummary, TerminalSurface};
use hl_ws_extension::{ExtensionName, Manifest, Record, Services, WorkspaceInfo};

use super::super::conversation::Conversation;
use super::super::sidecar::{Image, Sidecar, SidecarSpec};
use super::super::{Bridge, Extensions, Records};
use super::{Plan, Supply};
use crate::config::WorkspaceConfig;

/// The real supply: one workspace, its records, and its container daemon.
pub struct Workspace {
    workspace: WorkspaceConfig,
}

impl Workspace {
    /// Binds a supply to one workspace.
    #[must_use]
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            workspace: workspace.clone(),
        }
    }

    /// Where this workspace keeps its extension state and sockets.
    fn root(&self) -> PathBuf {
        self.workspace.storage_dir(&crate::paths::hl_root())
    }

    /// The socket one extension is given, in a directory of its own so
    /// [`SidecarSpec::prepare`] can confine it without touching anything else.
    fn socket(&self, name: &ExtensionName) -> PathBuf {
        self.root().join("extensions").join(format!("{name}.sock"))
    }

    /// The record of the one extension that should be running, if there is one.
    fn record(&self) -> Result<Option<Record>, String> {
        let storage = hl_ws::storage::Directory::open(self.root()).map_err(|error| error.to_string())?;
        let records = Records::open(storage).map_err(|fault| fault.to_string())?;
        let all = records.all().map_err(|fault| fault.to_string())?;
        Ok(all.into_iter().find(|record| record.enabled))
    }

    /// The workspace's own container daemon, started if it is not up.
    fn bridge(&self) -> Result<Arc<Bridge>, String> {
        let domain = crate::runtime::domain::Domain::new(&self.workspace);
        let socket = domain.ensure(&self.workspace).map_err(|error| error.to_string())?;
        Bridge::new(socket).map(Arc::new).map_err(|error| error.to_string())
    }

    /// What the extension's image says about how to run it.
    fn image(bridge: &Bridge, record: &Record) -> Result<Image, String> {
        let client = bridge.client();
        let inspection = bridge
            .wait(client.images().inspect(&record.image_digest))
            .map_err(|error| error.to_string())?;
        Ok(Image::from_inspection(record.image_digest.clone(), &inspection))
    }

    /// What this workspace tells an extension about itself.
    fn describe(&self) -> WorkspaceInfo {
        WorkspaceInfo {
            name: self.workspace.name.clone(),
            architecture: self.workspace.arch.as_str().to_owned(),
            image: self.workspace.image.clone(),
        }
    }
}

impl Supply for Workspace {
    /// # Errors
    /// Returns why the records, the container daemon, or the image could not be
    /// read. A workspace with nothing installed is `Ok(None)` and reaches no
    /// daemon at all.
    fn plan(&self) -> Result<Option<Plan>, String> {
        let Some(record) = self.record()? else {
            return Ok(None);
        };
        let manifest = described(&record);
        let bridge = self.bridge()?;
        let image = Self::image(&bridge, &record)?;
        let spec = SidecarSpec::new(&manifest, &record.granted, &image, self.socket(&record.name));
        Ok(Some(Plan {
            record,
            manifest,
            spec,
            workspace: self.workspace.name.clone(),
        }))
    }

    /// # Errors
    /// Returns a container daemon failure.
    fn ensure(&self, plan: &Plan) -> Result<(), String> {
        Sidecar::new(self.bridge()?)
            .ensure(&plan.spec)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// # Errors
    /// Returns why the conversation ended early, including the failure to bind
    /// the ports it is served against.
    fn attend(&self, _plan: &Plan, conversation: &mut Conversation) -> Result<(), String> {
        let extensions = Extensions::open(&self.workspace).map_err(|error| error.to_string())?;
        let console = Console;
        let services = Services {
            workspace: self.describe(),
            containers: extensions.containers(),
            control: extensions.control(),
            images: extensions.images(),
            terminal: &console,
            files: extensions.files(),
        };
        conversation.serve(&services).map_err(|fault| fault.to_string())
    }

    fn halt(&self, plan: &Plan) {
        let Ok(bridge) = self.bridge() else {
            return;
        };
        if let Err(error) = Sidecar::new(bridge).stop(plan.spec.container()) {
            hl_log::hl_error!(hl_log::tag::RUNTIME, "extension {}: {error}", plan.record.name);
        }
    }
}

/// The manifest a record stands for.
///
/// A record is written down; a manifest is what an image declares. Until the
/// two are stored together, the container is described from the record alone,
/// which is the conservative direction: it grants exactly what was consented
/// to and asks the image for its own entrypoint and user.
fn described(record: &Record) -> Manifest {
    Manifest {
        name: record.name.clone(),
        display_name: record.name.to_string(),
        version: String::new(),
        protocol: hl_ws_extension::PROTOCOL,
        capabilities: record.granted.clone(),
        entrypoint: None,
        activation: hl_ws_extension::Activation::default(),
        interface: None,
        resources: hl_ws_extension::Resources::default(),
        filesystem_roots: Vec::new(),
    }
}

/// The terminal an extension reaches from this host: none.
///
/// The terminal port belongs to the window that owns the surface, and this host
/// runs off the main loop. An extension asking for it is told plainly rather
/// than given an empty answer it would read as an empty workspace.
struct Console;

impl TerminalSurface for Console {
    fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
        Err(unreachable_terminal())
    }

    fn open_tab(&self, _title: &str) -> Result<String, HostError> {
        Err(unreachable_terminal())
    }

    fn split(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Err(unreachable_terminal())
    }

    fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
        Err(unreachable_terminal())
    }
}

/// Said the same way by every terminal call, so an extension can recognize it.
fn unreachable_terminal() -> HostError {
    HostError::Failed("the terminal is not reachable from the extension host".to_owned())
}
