//! The real supply: one workspace, its records, and its container daemon.
//!
//! Everything in this file needs a running workspace, which is why it is the
//! one part of the host the suite cannot drive. It is kept apart from the
//! orchestration for exactly that reason: what can be tested and what cannot
//! are not mixed in one file.

use std::path::PathBuf;
use std::sync::Arc;

use hl_extension::port::{
    Division, HostError, PaneText, TabSummary, TerminalSurface, WorkspaceInventory, WorkspaceState,
};
use hl_extension::{ExtensionName, Record, Services, WorkspaceInfo};

use super::super::conversation::Conversation;
use super::super::roster::described;
use super::super::sidecar::{Image, Sidecar, SidecarSpec};
use super::super::{Bridge, Extensions, Records};
use super::{Plan, Supply};
use crate::config::WorkspaceConfig;

impl super::Host {
    /// Starts hosting one named extension of one workspace.
    ///
    /// The terminal surface is the window's, because the terminal is widgets
    /// and this host runs off the main loop; a host given none tells an
    /// extension that asks so plainly.
    #[must_use]
    pub fn extension(
        workspace: &WorkspaceConfig,
        name: &ExtensionName,
        terminal: Arc<dyn TerminalSurface + Send + Sync>,
        audience: super::Audience,
    ) -> Self {
        Self::open(Workspace::extension(workspace, name).through(terminal), audience)
    }
}

/// The real supply: one workspace, its records, and its container daemon.
pub struct Workspace {
    config: WorkspaceConfig,
    /// Which extension this supply serves. `None` means whichever one is
    /// enabled, which is what a workspace with a single extension wants.
    wanted: Option<ExtensionName>,
    /// Where terminal calls are sent. `None` when no window offered one, in
    /// which case an extension asking is told so plainly.
    terminal: Option<Arc<dyn TerminalSurface + Send + Sync>>,
}

impl Workspace {
    /// Binds a supply to whichever extension of one workspace is enabled.
    #[must_use]
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            config: workspace.clone(),
            wanted: None,
            terminal: None,
        }
    }

    /// Binds a supply to one named extension of one workspace.
    ///
    /// Named rather than "whichever is enabled" because a workspace has a list
    /// of extensions and each one is drawn on a page of its own, so each needs
    /// a host that serves exactly its own record.
    #[must_use]
    pub fn extension(workspace: &WorkspaceConfig, name: &ExtensionName) -> Self {
        Self {
            config: workspace.clone(),
            wanted: Some(name.clone()),
            terminal: None,
        }
    }

    /// Points the supply's terminal calls at a surface the window owns.
    #[must_use]
    pub fn through(mut self, terminal: Arc<dyn TerminalSurface + Send + Sync>) -> Self {
        self.terminal = Some(terminal);
        self
    }

    /// Where this workspace keeps its extension state and sockets.
    fn root(&self) -> PathBuf {
        self.config.storage_dir(&crate::paths::hl_root())
    }

    /// The socket one extension is given, in a directory of its own so
    /// [`SidecarSpec::prepare`] can confine it without touching anything else.
    fn socket(&self, name: &ExtensionName) -> PathBuf {
        self.root().join("extensions").join(format!("{name}.sock"))
    }

    /// The record of the extension that should be running, if there is one.
    ///
    /// A named supply serves only its own record and only while it is enabled,
    /// so disabling an extension takes its host down rather than leaving a
    /// sidecar running behind a page that no longer offers it.
    fn record(&self) -> Result<Option<Record>, String> {
        let storage = hl_ws::storage::Directory::open(self.root()).map_err(|error| error.to_string())?;
        let records = Records::open(storage).map_err(|fault| fault.to_string())?;
        let all = records.all().map_err(|fault| fault.to_string())?;
        let wanted = self.wanted.as_ref();
        Ok(all
            .into_iter()
            .find(|record| record.enabled && wanted.is_none_or(|name| *name == record.name)))
    }

    /// The workspace's own container daemon, started if it is not up.
    fn bridge(&self) -> Result<Arc<Bridge>, String> {
        let domain = crate::runtime::domain::Domain::new(&self.config);
        let socket = domain.ensure(&self.config).map_err(|error| error.to_string())?;
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
            name: self.config.name.clone(),
            architecture: self.config.arch.as_str().to_owned(),
            image: self.config.image.clone(),
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
            workspace: self.config.name.clone(),
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
        let extensions = Extensions::open(&self.config).map_err(|error| error.to_string())?;
        let console = Console;
        let terminal: &dyn TerminalSurface = self.terminal.as_deref().unwrap_or(&console);
        let store = Store {
            current: self.config.name.clone(),
        };
        let services = Services {
            workspace: self.describe(),
            workspaces: &store,
            containers: extensions.containers(),
            control: extensions.control(),
            images: extensions.images(),
            terminal,
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

/// The store of workspaces this user has, and whether each one is up.
///
/// Read from the store on each call rather than captured once: a workspace
/// created after this extension started is a workspace that exists, and an
/// answer from a stale copy would say otherwise.
struct Store {
    /// The workspace the asking extension is hosted by.
    current: String,
}

impl Store {
    /// Whether one workspace's execution domain is accepting connections.
    ///
    /// Connecting is the only honest test: a socket file outlives the process
    /// that bound it, so its presence says nothing about what is running.
    fn running(workspace: &WorkspaceConfig) -> bool {
        let socket = crate::runtime::domain::Domain::new(workspace).socket();
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }
}

impl WorkspaceInventory for Store {
    /// # Errors
    /// Returns a host failure when the workspace store cannot be read.
    fn workspaces(&self) -> Result<Vec<WorkspaceState>, HostError> {
        let path = crate::paths::hl_root().join("workspaces.conf");
        let store = crate::config::WorkspaceStore::load(path).map_err(|error| HostError::Failed(error.to_string()))?;
        Ok(store
            .all()
            .iter()
            .map(|workspace| WorkspaceState {
                name: workspace.name.clone(),
                architecture: workspace.arch.as_str().to_owned(),
                image: workspace.image.clone(),
                running: Self::running(workspace),
                current: workspace.name == self.current,
            })
            .collect())
    }
}

/// The terminal an extension reaches when no window offered one.
///
/// The terminal port belongs to the window that owns the surface, and this host
/// runs off the main loop. A host started with no window behind it tells an
/// extension so plainly rather than giving an empty answer it would read as an
/// empty workspace.
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

    fn read(&self, _slot: &str, _lines: usize) -> Result<PaneText, HostError> {
        Err(unreachable_terminal())
    }

    fn close(&self, _slot: &str) -> Result<(), HostError> {
        Err(unreachable_terminal())
    }

    fn focus(&self, _slot: &str) -> Result<(), HostError> {
        Err(unreachable_terminal())
    }

    fn ratio(&self, _slot: &str, _ratio: f64) -> Result<(), HostError> {
        Err(unreachable_terminal())
    }

    fn surface(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Err(unreachable_terminal())
    }
}

/// Said the same way by every terminal call, so an extension can recognize it.
fn unreachable_terminal() -> HostError {
    HostError::Failed("the terminal is not reachable from the extension host".to_owned())
}
