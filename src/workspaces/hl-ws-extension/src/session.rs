//! One connected extension: what it may reach, and what it is subscribed to.

use std::collections::BTreeSet;

use crate::authority::Authority;
use crate::capability::Capability;
use crate::port::{ContainerControl, ContainerInventory, ImageStore, TerminalSurface, WorkspaceFiles};
use crate::request::{Failure, Reply, Request, Topic, WorkspaceInfo};

/// The host services a session dispatches to.
///
/// A borrowed bundle rather than an omnibus trait: each field is a separate
/// narrow port, and a dispatcher still cannot touch one without going through
/// [`Authority`].
pub struct Services<'a> {
    pub workspace: WorkspaceInfo,
    pub containers: &'a dyn ContainerInventory,
    pub control: &'a dyn ContainerControl,
    pub images: &'a dyn ImageStore,
    pub terminal: &'a dyn TerminalSurface,
    pub files: &'a dyn WorkspaceFiles,
}

/// One connected extension.
pub struct Session {
    authority: Authority,
    topics: BTreeSet<Topic>,
    tab: Option<String>,
    pending: Vec<hl_gui::Frame>,
}

impl Session {
    #[must_use]
    pub fn new(authority: Authority) -> Self {
        Self {
            authority,
            topics: BTreeSet::new(),
            tab: None,
            pending: Vec::new(),
        }
    }

    #[must_use]
    pub const fn authority(&self) -> &Authority {
        &self.authority
    }

    #[must_use]
    pub fn authority_mut(&mut self) -> &mut Authority {
        &mut self.authority
    }

    /// Topics this session currently follows.
    #[must_use]
    pub fn topics(&self) -> Vec<Topic> {
        self.topics.iter().copied().collect()
    }

    /// The tab this session owns, if it has opened one.
    #[must_use]
    pub fn tab(&self) -> Option<&str> {
        self.tab.as_deref()
    }

    /// Whether an emission on `topic` may still be delivered.
    ///
    /// Checked on every emission rather than only at subscribe time, so
    /// revoking a capability stops an established stream.
    #[must_use]
    pub fn may_emit(&self, topic: Topic) -> bool {
        self.topics.contains(&topic) && self.authority.permit(topic.capability()).is_ok()
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
            Some(path) => self.authority.permit_path(capability, path)?,
            None => self.authority.permit(capability)?,
        }
        self.serve(request, services)
    }

    fn serve(&mut self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        match request {
            Request::WorkspaceInfo => Ok(Reply::Workspace(services.workspace.clone())),
            Request::ContainerList | Request::ContainerInspect { .. } => self.containers(request, services),
            Request::ContainerCreate { .. }
            | Request::ContainerStart { .. }
            | Request::ContainerStop { .. }
            | Request::ContainerRemove { .. } => self.control(request, services),
            Request::ImageList | Request::ImagePull { .. } => self.images(request, services),
            Request::TerminalTabs
            | Request::TerminalOpenTab { .. }
            | Request::TerminalSplit { .. }
            | Request::TerminalSpawn { .. } => self.terminal(request, services),
            Request::FilesystemList { .. } | Request::FilesystemRead { .. } | Request::FilesystemWrite { .. } => {
                self.files(request, services)
            }
            Request::InterfaceOpenTab { title } => self.open_tab(title, services),
            Request::InterfaceRender { frame } => self.render(frame),
            Request::EventSubscribe { topic } => {
                self.topics.insert(*topic);
                Ok(Reply::Done)
            }
            Request::EventUnsubscribe { topic } => {
                self.topics.remove(topic);
                Ok(Reply::Done)
            }
        }
    }

    fn containers(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self.authority.port(Capability::ContainerRead, services.containers)?;
        if let Request::ContainerInspect { id } = request {
            return Ok(Reply::Container(port.inspect(id)?));
        }
        Ok(Reply::Containers(port.list()?))
    }

    fn control(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self.authority.port(Capability::ContainerControl, services.control)?;
        match request {
            Request::ContainerCreate { image, name } => Ok(Reply::Identity(port.create(image, name)?)),
            Request::ContainerStart { id } => port.start(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerStop { id } => port.stop(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerRemove { id } => port.remove(id).map(|()| Reply::Done).map_err(Failure::from),
            _ => Err(Failure::Unsupported {
                call: "container control".into(),
            }),
        }
    }

    fn images(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if let Request::ImagePull { reference } = request {
            let port = self.authority.port(Capability::ImageWrite, services.images)?;
            return Ok(Reply::Image(port.pull(reference)?));
        }
        let port = self.authority.port(Capability::ImageRead, services.images)?;
        Ok(Reply::Images(port.list()?))
    }

    fn terminal(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if matches!(request, Request::TerminalTabs) {
            let port = self.authority.port(Capability::TerminalRead, services.terminal)?;
            return Ok(Reply::Tabs(port.tabs()?));
        }
        let port = self.authority.port(Capability::TerminalControl, services.terminal)?;
        Self::command(request, port.port())
    }

    fn command(request: &Request, port: &dyn TerminalSurface) -> Result<Reply, Failure> {
        match request {
            Request::TerminalOpenTab { title } => Ok(Reply::Identity(port.open_tab(title)?)),
            Request::TerminalSplit { slot, division } => Ok(Reply::Identity(port.split(slot, *division)?)),
            Request::TerminalSpawn { slot, command } => {
                port.spawn(slot, command).map(|()| Reply::Done).map_err(Failure::from)
            }
            _ => Err(Failure::Unsupported {
                call: "terminal command".into(),
            }),
        }
    }

    fn files(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        match request {
            Request::FilesystemList { path } => {
                let port = self.authority.port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Entries(port.list(path)?))
            }
            Request::FilesystemRead { path } => {
                let port = self.authority.port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Contents(port.read(path)?))
            }
            Request::FilesystemWrite { path, contents } => {
                let port = self.authority.port(Capability::FilesystemWrite, services.files)?;
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
        let port = self.authority.port(Capability::Interface, services.terminal)?;
        let id = port.open_tab(title)?;
        self.tab = Some(id.clone());
        Ok(Reply::Identity(id))
    }
}
