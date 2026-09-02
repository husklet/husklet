//! One connected extension: what it may reach, and what it is subscribed to.
//!
//! The bookkeeping — the authority, the topics followed, revocation — is
//! `hl-rpc`'s [`hl_rpc::Session`]. What a call means is this domain's, and lives
//! here: [`Session::dispatch`] is the one place a workspace call reaches a
//! workspace service, always through a capability check.

use hl_rpc::Authority;

use crate::capability::Capability;
use crate::port::{
    pane_lines, ContainerControl, ContainerInventory, Division, ImageStore, TerminalSurface, WorkspaceControl,
    WorkspaceFiles, WorkspaceInventory,
};
use crate::request::{Failure, Reply, Request, Topic, WorkspaceInfo};

/// The host services a session dispatches to.
///
/// A borrowed bundle rather than an omnibus trait: each field is a separate
/// narrow port, and a dispatcher still cannot touch one without going through
/// [`Authority`].
pub struct Services<'a> {
    pub workspace: WorkspaceInfo,
    pub workspaces: &'a dyn WorkspaceInventory,
    pub workspace_control: &'a dyn WorkspaceControl,
    pub containers: &'a dyn ContainerInventory,
    pub control: &'a dyn ContainerControl,
    pub images: &'a dyn ImageStore,
    pub terminal: &'a dyn TerminalSurface,
    pub files: &'a dyn WorkspaceFiles,
}

/// One connected extension.
pub struct Session {
    peer: hl_rpc::Session<Topic>,
    tab: Option<String>,
    pending: Vec<hl_gui::Frame>,
    mutations: Vec<hl_gui::SourceMutation>,
}

impl std::ops::Deref for Session {
    type Target = hl_rpc::Session<Topic>;

    fn deref(&self) -> &Self::Target {
        &self.peer
    }
}

impl std::ops::DerefMut for Session {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.peer
    }
}

impl Session {
    /// A session for an extension with the given authority.
    #[must_use]
    pub fn new(authority: Authority) -> Self {
        Self {
            peer: hl_rpc::Session::new(authority),
            tab: None,
            pending: Vec::new(),
            mutations: Vec::new(),
        }
    }

    /// The tab this session owns, if it has opened one.
    #[must_use]
    pub fn tab(&self) -> Option<&str> {
        self.tab.as_deref()
    }

    /// Handles one call.
    ///
    /// The capability is checked first, and a path-bearing call is confined to
    /// the declared roots, before any service is reached.
    ///
    /// # Errors
    /// Returns a refusal, or whatever the host service reported.
    pub fn dispatch(&mut self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let capability = request.capability();
        match request.path() {
            Some(path) => self.peer.authority().permit_path(capability, path)?,
            None => self.peer.authority().permit(capability)?,
        }
        self.serve(request, services)
    }

    fn serve(&mut self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        match request {
            Request::WorkspaceInfo => Ok(Reply::Workspace(services.workspace.clone())),
            Request::WorkspaceList => self.workspaces(services),
            Request::WorkspaceInspect { .. }
            | Request::WorkspaceCreate { .. }
            | Request::WorkspaceUpdate { .. }
            | Request::WorkspaceDelete { .. }
            | Request::WorkspaceStart { .. }
            | Request::WorkspaceStop { .. }
            | Request::WorkspaceRestart { .. } => self.workspace_control(request, services),
            Request::ContainerList
            | Request::ContainerInspect { .. }
            | Request::ContainerProcesses { .. }
            | Request::ContainerLogs { .. }
            | Request::ExecutionInspect { .. } => self.containers(request, services),
            Request::ContainerCreate { .. }
            | Request::ContainerStart { .. }
            | Request::ContainerStop { .. }
            | Request::ContainerRemove { .. }
            | Request::ContainerPause { .. }
            | Request::ContainerUnpause { .. }
            | Request::ContainerRestart { .. }
            | Request::ContainerKill { .. }
            | Request::ContainerExec { .. } => self.control(request, services),
            Request::ImageList | Request::ImagePull { .. } => self.images(request, services),
            Request::TerminalTabs
            | Request::TerminalOpenTab { .. }
            | Request::TerminalSplit { .. }
            | Request::TerminalSpawn { .. }
            | Request::TerminalReadPane { .. }
            | Request::TerminalClosePane { .. }
            | Request::TerminalFocusPane { .. }
            | Request::TerminalRatio { .. } => self.terminal(request, services),
            Request::FilesystemList { .. } | Request::FilesystemRead { .. } | Request::FilesystemWrite { .. } => {
                self.files(request, services)
            }
            Request::InterfaceOpenTab { title } => self.open_tab(title, services),
            Request::InterfaceSplit { slot, division } => self.open_pane(slot, *division, services),
            Request::InterfaceRender { frame } => self.render(frame),
            Request::SourceResize { mutation } => self.mutate(mutation.clone()),
            Request::EventSubscribe { topic } => {
                self.peer.follow(*topic);
                Ok(Reply::Done)
            }
            Request::EventUnsubscribe { topic } => {
                self.peer.unfollow(*topic);
                Ok(Reply::Done)
            }
        }
    }

    fn containers(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self
            .peer
            .authority()
            .port(Capability::ContainerRead, services.containers)?;
        match request {
            Request::ContainerInspect { id } => Ok(Reply::Container(port.inspect(id)?)),
            Request::ContainerProcesses { id } => Ok(Reply::Processes(port.processes(id)?)),
            Request::ContainerLogs { id, stdout, stderr } => Ok(Reply::Logs(port.logs(id, *stdout, *stderr)?)),
            Request::ExecutionInspect { id } => Ok(Reply::Execution(port.execution(id)?)),
            Request::ContainerList => Ok(Reply::Containers(port.list()?)),
            _ => Err(Failure::Unsupported {
                call: "container read".into(),
            }),
        }
    }

    fn control(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self
            .peer
            .authority()
            .port(Capability::ContainerControl, services.control)?;
        match request {
            Request::ContainerCreate { image, name } => Ok(Reply::Identity(port.create(image, name)?)),
            Request::ContainerStart { id } => port.start(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerStop { id } => port.stop(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerRemove { id } => port.remove(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerPause { id } => port.pause(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerUnpause { id } => port.unpause(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerRestart { id } => port.restart(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerKill { id, signal } => port.kill(id, signal).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerExec {
                id,
                command,
                user,
                working_directory,
            } => Ok(Reply::Identity(port.execute(
                id,
                command,
                user.as_deref(),
                working_directory.as_deref(),
            )?)),
            _ => Err(Failure::Unsupported {
                call: "container control".into(),
            }),
        }
    }

    fn images(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if let Request::ImagePull { reference } = request {
            let port = self.peer.authority().port(Capability::ImageWrite, services.images)?;
            return Ok(Reply::Image(port.pull(reference)?));
        }
        let port = self.peer.authority().port(Capability::ImageRead, services.images)?;
        Ok(Reply::Images(port.list()?))
    }

    /// Every workspace the host knows of.
    fn workspaces(&self, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self
            .peer
            .authority()
            .port(Capability::WorkspaceRead, services.workspaces)?;
        Ok(Reply::Workspaces(port.workspaces()?))
    }

    fn workspace_control(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let capability = request.capability();
        let port = self.peer.authority().port(capability, services.workspace_control)?;
        match request {
            Request::WorkspaceInspect { name } => Ok(Reply::WorkspaceConfiguration(port.inspect(name)?)),
            Request::WorkspaceCreate { configuration } => {
                Ok(Reply::WorkspaceConfiguration(port.create(configuration)?))
            }
            Request::WorkspaceUpdate { name, configuration } => {
                Ok(Reply::WorkspaceConfiguration(port.update(name, configuration)?))
            }
            Request::WorkspaceDelete { name } => port.delete(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::WorkspaceStart { name } => port.start(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::WorkspaceStop { name } => port.stop(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::WorkspaceRestart { name } => port.restart(name).map(|()| Reply::Done).map_err(Failure::from),
            _ => Err(Failure::Unsupported {
                call: "workspace control".into(),
            }),
        }
    }

    fn terminal(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if matches!(request, Request::TerminalTabs) {
            let port = self
                .peer
                .authority()
                .port(Capability::TerminalRead, services.terminal)?;
            return Ok(Reply::Tabs(port.tabs()?));
        }
        if let Request::TerminalReadPane { slot, lines } = request {
            return self.text(slot, *lines, services);
        }
        let port = self
            .peer
            .authority()
            .port(Capability::TerminalControl, services.terminal)?;
        Self::command(request, port.port())
    }

    fn command(request: &Request, port: &dyn TerminalSurface) -> Result<Reply, Failure> {
        match request {
            Request::TerminalOpenTab { title } => Ok(Reply::Identity(port.open_tab(title)?)),
            Request::TerminalSplit { slot, division } => Ok(Reply::Identity(port.split(slot, *division)?)),
            Request::TerminalSpawn { slot, command } => {
                port.spawn(slot, command).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalClosePane { slot } => port.close(slot).map(|()| Reply::Done).map_err(Failure::from),
            Request::TerminalFocusPane { slot } => port.focus(slot).map(|()| Reply::Done).map_err(Failure::from),
            Request::TerminalRatio { slot, ratio } => {
                port.ratio(slot, *ratio).map(|()| Reply::Done).map_err(Failure::from)
            }
            _ => Err(Failure::Unsupported {
                call: "terminal command".into(),
            }),
        }
    }

    /// Reads a bounded tail of one pane's text.
    ///
    /// The bound is applied here rather than trusted to the caller, so a host
    /// implementation cannot be talked into extracting a whole scrollback by an
    /// extension that asks for one.
    fn text(&self, slot: &str, lines: Option<usize>, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self
            .peer
            .authority()
            .port(Capability::TerminalOutput, services.terminal)?;
        Ok(Reply::Text(port.read(slot, pane_lines(lines))?))
    }

    fn files(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        match request {
            Request::FilesystemList { path } => {
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Entries(port.list(path)?))
            }
            Request::FilesystemRead { path } => {
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Contents(port.read(path)?))
            }
            Request::FilesystemWrite { path, contents } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.write(path, contents).map(|()| Reply::Done).map_err(Failure::from)
            }
            _ => Err(Failure::Unsupported {
                call: "filesystem".into(),
            }),
        }
    }

    /// Accepts an interface description for the session's own tab.
    ///
    /// The frame is handed to the surface the host owns; an extension that has
    /// not opened a tab has nowhere to draw, which is a conflict rather than a
    /// refusal, because the grant is present and only the order is wrong.
    fn render(&mut self, frame: &hl_gui::Frame) -> Result<Reply, Failure> {
        if self.tab.is_none() {
            return Err(Failure::Conflict {
                detail: "no tab is open to render into".into(),
            });
        }
        self.pending.push(frame.clone());
        Ok(Reply::Done)
    }

    /// Accepts a change to a windowed source the session's tables draw from.
    fn mutate(&mut self, mutation: hl_gui::SourceMutation) -> Result<Reply, Failure> {
        if self.tab.is_none() {
            return Err(Failure::Conflict {
                detail: "no tab is open to hold a source".into(),
            });
        }
        self.mutations.push(mutation);
        Ok(Reply::Done)
    }

    /// Source changes received since the host last collected them.
    #[must_use]
    pub fn drain_sources(&mut self) -> Vec<hl_gui::SourceMutation> {
        std::mem::take(&mut self.mutations)
    }

    /// Interface frames received since the host last collected them.
    ///
    /// The protocol layer holds them rather than applying them: it owns no
    /// toolkit, and the surface belongs to the host.
    #[must_use]
    pub fn drain(&mut self) -> Vec<hl_gui::Frame> {
        std::mem::take(&mut self.pending)
    }

    /// Opens the one tab a session owns. A second call returns the same tab
    /// rather than accumulating surfaces the extension has forgotten about.
    fn open_tab(&mut self, title: &str, services: &Services<'_>) -> Result<Reply, Failure> {
        if let Some(existing) = &self.tab {
            return Ok(Reply::Identity(existing.clone()));
        }
        let port = self.peer.authority().port(Capability::Interface, services.terminal)?;
        let id = port.open_tab(title)?;
        self.tab = Some(id.clone());
        Ok(Reply::Identity(id))
    }

    /// Divides a pane and takes the new one as the surface this session draws
    /// into.
    ///
    /// A session has one interface, so the pane replaces whatever surface it was
    /// drawing on rather than becoming a second one: two trees fed by one stream
    /// of reconciliation frames would disagree the moment either missed a frame.
    fn open_pane(&mut self, slot: &str, division: Division, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self.peer.authority().port(Capability::Interface, services.terminal)?;
        let id = port.surface(slot, division)?;
        self.tab = Some(id.clone());
        Ok(Reply::Identity(id))
    }
}
