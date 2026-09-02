//! One connected extension: what it may reach, and what it is subscribed to.
//!
//! The bookkeeping — the authority, the topics followed, revocation — is
//! `hl-rpc`'s [`hl_rpc::Session`]. What a call means is this domain's, and lives
//! here: [`Session::dispatch`] is the one place a workspace call reaches a
//! workspace service, always through a capability check.

use hl_rpc::Authority;

use crate::capability::Capability;
use crate::port::{
    pane_lines, ContainerControl, ContainerInventory, Division, ExtensionStore, GridSize, ImageStore, NetworkStore,
    TerminalSurface, VolumeStore, WorkspaceControl, WorkspaceFiles, WorkspaceInventory, PANE_GRID_EDGE,
    PANE_INPUT_BYTES,
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
    pub extensions: &'a dyn ExtensionStore,
    pub containers: &'a dyn ContainerInventory,
    pub control: &'a dyn ContainerControl,
    pub images: &'a dyn ImageStore,
    pub volumes: &'a dyn VolumeStore,
    pub networks: &'a dyn NetworkStore,
    pub terminal: &'a dyn TerminalSurface,
    pub files: &'a dyn WorkspaceFiles,
}

/// One connected extension.
pub struct Session {
    peer: hl_rpc::Session<Topic>,
    surfaces: std::collections::BTreeSet<String>,
    pending: Vec<SurfaceFrame>,
    mutations: Vec<SurfaceMutation>,
}

/// One reconciliation frame and the surface that owns its sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceFrame {
    /// Stable workspace pane identity.
    pub slot: String,
    /// Frame whose sequence is local to `slot`.
    pub frame: hl_gui::Frame,
}

/// One data-source mutation and the surface whose table owns it.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceMutation {
    /// Stable workspace pane identity.
    pub slot: String,
    /// Mutation applied only to the addressed surface.
    pub mutation: hl_gui::SourceMutation,
}

/// One interaction and the independently addressed surface that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceEvent {
    pub slot: String,
    pub event: hl_gui::Event,
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
            surfaces: std::collections::BTreeSet::new(),
            pending: Vec::new(),
            mutations: Vec::new(),
        }
    }

    /// The tab this session owns, if it has opened one.
    #[must_use]
    pub fn tab(&self) -> Option<&str> {
        if self.surfaces.len() == 1 {
            self.surfaces.iter().next().map(String::as_str)
        } else {
            None
        }
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
        match request {
            Request::FilesystemRename { from, to } => {
                self.peer.authority().permit_path(capability, from)?;
                self.peer.authority().permit_path(capability, to)?;
            }
            _ => match request.path() {
                Some(path) => self.peer.authority().permit_path(capability, path)?,
                None => self.peer.authority().permit(capability)?,
            },
        }
        self.serve(request, services)
    }

    fn serve(&mut self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        match request {
            Request::WorkspaceInfo => Ok(Reply::Workspace(services.workspace.clone())),
            Request::WorkspaceList => self.workspaces(services),
            Request::WorkspaceInspect { .. }
            | Request::WorkspaceCreate { .. }
            | Request::WorkspaceAdopt { .. }
            | Request::WorkspaceUpdate { .. }
            | Request::WorkspaceDelete { .. }
            | Request::WorkspaceStart { .. }
            | Request::WorkspaceStop { .. }
            | Request::WorkspaceRestart { .. } => self.workspace_control(request, services),
            Request::ExtensionList
            | Request::ExtensionInspect { .. }
            | Request::ExtensionEnable { .. }
            | Request::ExtensionDisable { .. }
            | Request::ExtensionRemove { .. }
            | Request::ExtensionAcquisitionStart { .. }
            | Request::ExtensionAcquisitionStatus { .. }
            | Request::ExtensionAcquisitionCancel { .. }
            | Request::ExtensionInstall { .. }
            | Request::ExtensionUpdate { .. } => self.extensions(request, services),
            Request::ContainerList
            | Request::ContainerInspect { .. }
            | Request::ContainerProcesses { .. }
            | Request::ContainerLogs { .. }
            | Request::ExecutionInspect { .. }
            | Request::ExecutionList
            | Request::ExecutionLogs { .. }
            | Request::ExecutionWait { .. } => self.containers(request, services),
            Request::ContainerAttachTerminal { id, command } => {
                immutable_identity(id, &[32, 64], "container")?;
                validate_terminal_command(command)?;
                let port = self
                    .peer
                    .authority()
                    .port(Capability::ContainerAttach, services.terminal)?;
                Ok(Reply::Identity(port.attach_container(id, command)?))
            }
            Request::ContainerCreate { .. }
            | Request::ContainerStart { .. }
            | Request::ContainerStop { .. }
            | Request::ContainerRemove { .. }
            | Request::ContainerPause { .. }
            | Request::ContainerUnpause { .. }
            | Request::ContainerRestart { .. }
            | Request::ContainerKill { .. }
            | Request::ExecutionKill { .. }
            | Request::ExecutionRemove { .. }
            | Request::ContainerExec { .. } => self.control(request, services),
            Request::ImageList
            | Request::ImagePull { .. }
            | Request::ImagePullStart { .. }
            | Request::ImagePullStatus { .. }
            | Request::ImagePullCancel { .. }
            | Request::ImageInspect { .. }
            | Request::ImageRemove { .. }
            | Request::ImagePrune => self.images(request, services),
            Request::VolumeList
            | Request::VolumeInspect { .. }
            | Request::VolumeCreate { .. }
            | Request::VolumeRemove { .. } => self.volumes(request, services),
            Request::NetworkList
            | Request::NetworkInspect { .. }
            | Request::NetworkCreate { .. }
            | Request::NetworkRemove { .. }
            | Request::NetworkConnect { .. }
            | Request::NetworkDisconnect { .. } => self.networks(request, services),
            Request::TerminalTabs
            | Request::TerminalTopology
            | Request::TerminalOpenTab { .. }
            | Request::TerminalSplit { .. }
            | Request::TerminalSpawn { .. }
            | Request::TerminalReadPane { .. }
            | Request::TerminalWritePane { .. }
            | Request::TerminalResizeGrid { .. }
            | Request::TerminalClosePane { .. }
            | Request::TerminalFocusPane { .. }
            | Request::TerminalRatio { .. } => self.terminal(request, services),
            Request::PaneList => {
                let port = self.peer.authority().port(Capability::PaneObserve, services.terminal)?;
                Ok(Reply::Panes(port.pane_inventory()?))
            }
            Request::PaneSemanticRead { slot } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::PaneSemanticRead, services.terminal)?;
                Ok(Reply::Semantics(port.semantics(slot)?))
            }
            Request::PaneSemanticAction { slot, action } => {
                if action
                    .value
                    .as_ref()
                    .is_some_and(|value| value.len() > crate::port::SEMANTIC_ACTION_VALUE_LIMIT)
                {
                    return Err(Failure::Conflict {
                        detail: "pane semantic action value exceeds 4096 bytes".into(),
                    });
                }
                let port = self
                    .peer
                    .authority()
                    .port(Capability::PaneSemanticControl, services.terminal)?;
                let requirement = port.semantic_requirement(slot, action.node).map_err(Failure::from)?;
                self.peer.authority().port(requirement, services.terminal)?;
                port.semantic_action(slot, action)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::FilesystemList { .. }
            | Request::FilesystemRead { .. }
            | Request::FilesystemStat { .. }
            | Request::FilesystemWrite { .. }
            | Request::FilesystemMkdir { .. }
            | Request::FilesystemRename { .. }
            | Request::FilesystemRemove { .. } => self.files(request, services),
            Request::InterfaceOpenTab { title } => self.open_tab(title, services),
            Request::InterfaceSplit { slot, division } => self.open_pane(slot, *division, services),
            Request::InterfaceWithdraw { slot } => self.withdraw(slot, services),
            Request::InterfaceRender { frame } => self.render_legacy(frame),
            Request::InterfaceRenderAt { slot, frame } => self.render(slot, frame),
            Request::SourceResize { mutation } => self.mutate_legacy(mutation.clone()),
            Request::SourceResizeAt { slot, mutation } => self.mutate(slot, mutation.clone()),
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
            Request::ExecutionList => Ok(Reply::Executions(port.executions()?)),
            Request::ExecutionLogs { id, stdout, stderr } => {
                if !stdout && !stderr {
                    return Err(Failure::Conflict {
                        detail: "execution logs require stdout or stderr".into(),
                    });
                }
                Ok(Reply::Logs(port.execution_logs(id, *stdout, *stderr)?))
            }
            Request::ExecutionWait { id, timeout_ms } => {
                if !(1..=30_000).contains(timeout_ms) {
                    return Err(Failure::Conflict {
                        detail: "execution wait timeout_ms must be between 1 and 30000".into(),
                    });
                }
                Ok(Reply::Execution(port.execution_wait(id, *timeout_ms)?))
            }
            Request::ContainerList => Ok(Reply::Containers(port.list()?)),
            _ => Err(Failure::Unsupported {
                call: "container read".into(),
            }),
        }
    }

    fn control(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if let Request::ContainerCreate { spec } = request {
            if spec.mounts.iter().any(|mount| mount.read_only) {
                self.peer.authority().permit(Capability::VolumeRead)?;
            }
            if spec.mounts.iter().any(|mount| !mount.read_only) {
                self.peer.authority().permit(Capability::VolumeWrite)?;
            }
            if spec.network.is_some() || spec.ports.iter().any(|port| port.host.is_some()) {
                self.peer.authority().permit(Capability::NetworkWrite)?;
            }
        }
        let port = self
            .peer
            .authority()
            .port(Capability::ContainerControl, services.control)?;
        match request {
            Request::ContainerCreate { spec } => {
                validate_container_create(spec)?;
                Ok(Reply::Identity(port.create_spec(spec)?))
            }
            Request::ContainerStart { id } => port.start(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerStop { id } => {
                immutable_identity(id, &[32, 64], "container")?;
                port.stop(id).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::ContainerRemove { id } => {
                immutable_identity(id, &[32, 64], "container")?;
                port.remove(id).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::ContainerPause { id } => port.pause(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerUnpause { id } => port.unpause(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerRestart { id } => port.restart(id).map(|()| Reply::Done).map_err(Failure::from),
            Request::ContainerKill { id, signal } => {
                bounded_signal(signal)?;
                immutable_identity(id, &[32, 64], "container")?;
                port.kill(id, signal).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::ExecutionKill { id, signal } => {
                bounded_signal(signal)?;
                immutable_identity(id, &[32], "execution")?;
                port.execution_kill(id, signal)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExecutionRemove { id } => port.execution_remove(id).map(|()| Reply::Done).map_err(Failure::from),
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
        let port = self.peer.authority().port(request.capability(), services.images)?;
        match request {
            Request::ImageList => Ok(Reply::Images(port.list()?)),
            Request::ImagePull { reference } => Ok(Reply::Image(port.pull(reference)?)),
            Request::ImagePullStart { reference } => Ok(Reply::ImagePullJob(port.pull_start(reference)?)),
            Request::ImagePullStatus { job } => Ok(Reply::ImagePull(port.pull_status(job)?)),
            Request::ImagePullCancel { job } => port.pull_cancel(job).map(|()| Reply::Done).map_err(Failure::from),
            Request::ImageInspect { reference } => Ok(Reply::ImageDetails(port.inspect(reference)?)),
            Request::ImageRemove { reference } => {
                immutable_digest(reference, "image")?;
                port.remove(reference).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::ImagePrune => Ok(Reply::ImagePrune(port.prune()?)),
            _ => Err(Failure::Unsupported {
                call: "image operation".into(),
            }),
        }
    }

    fn volumes(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let capability = request.capability();
        let port = self.peer.authority().port(capability, services.volumes)?;
        match request {
            Request::VolumeList => Ok(Reply::Volumes(port.list()?)),
            Request::VolumeInspect { name } => Ok(Reply::Volume(port.inspect(name)?)),
            Request::VolumeCreate { name } => Ok(Reply::Volume(port.create(name)?)),
            Request::VolumeRemove { name, generation } => {
                immutable_identity(generation, &[32], "volume generation")?;
                port.remove(name, generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            _ => unreachable!(),
        }
    }

    fn networks(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let capability = request.capability();
        let port = self.peer.authority().port(capability, services.networks)?;
        match request {
            Request::NetworkList => Ok(Reply::Networks(port.list()?)),
            Request::NetworkInspect { reference } => Ok(Reply::Network(port.inspect(reference)?)),
            Request::NetworkCreate { name } => Ok(Reply::Identity(port.create(name)?)),
            Request::NetworkRemove { reference } => {
                immutable_identity(reference, &[32], "network")?;
                port.remove(reference).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::NetworkConnect { reference, container } => port
                .connect(
                    immutable_reference(reference, &[32], "network")?,
                    immutable_reference(container, &[32, 64], "container")?,
                )
                .map(|()| Reply::Done)
                .map_err(Failure::from),
            Request::NetworkDisconnect { reference, container } => port
                .disconnect(
                    immutable_reference(reference, &[32], "network")?,
                    immutable_reference(container, &[32, 64], "container")?,
                )
                .map(|()| Reply::Done)
                .map_err(Failure::from),
            _ => unreachable!(),
        }
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
            Request::WorkspaceAdopt { configuration } => {
                Ok(Reply::WorkspaceConfiguration(port.adopt(configuration)?))
            }
            Request::WorkspaceUpdate {
                name,
                generation,
                configuration,
            } => {
                immutable_identity(generation, &[32], "workspace generation")?;
                Ok(Reply::WorkspaceConfiguration(port.update(
                    name,
                    generation,
                    configuration,
                )?))
            }
            Request::WorkspaceDelete { name, generation } => {
                immutable_identity(generation, &[32], "workspace generation")?;
                port.delete(name, generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::WorkspaceStart { name } => port.start(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::WorkspaceStop { name } => port.stop(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::WorkspaceRestart { name } => port.restart(name).map(|()| Reply::Done).map_err(Failure::from),
            _ => Err(Failure::Unsupported {
                call: "workspace control".into(),
            }),
        }
    }

    fn extensions(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self.peer.authority().port(request.capability(), services.extensions)?;
        match request {
            Request::ExtensionList => Ok(Reply::Extensions(port.list()?)),
            Request::ExtensionInspect { name } => Ok(Reply::Extension(port.inspect(name)?)),
            Request::ExtensionEnable { name } => port.enable(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::ExtensionDisable { name } => port.disable(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::ExtensionRemove { name } => port.remove(name).map(|()| Reply::Done).map_err(Failure::from),
            Request::ExtensionAcquisitionStart { reference } => {
                acquisition_reference(reference)?;
                Ok(Reply::ExtensionAcquisitionJob(port.acquisition_start(reference)?))
            }
            Request::ExtensionAcquisitionStatus { job } => {
                acquisition_job(job)?;
                Ok(Reply::ExtensionAcquisition(port.acquisition_status(job)?))
            }
            Request::ExtensionAcquisitionCancel { job } => {
                acquisition_job(job)?;
                port.acquisition_cancel(job)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExtensionInstall { job, revision, granted } => {
                acquisition_job(job)?;
                Ok(Reply::Extension(port.install(job, *revision, granted)?))
            }
            Request::ExtensionUpdate { job, revision, granted } => {
                acquisition_job(job)?;
                Ok(Reply::Extension(port.update(job, *revision, granted)?))
            }
            _ => Err(Failure::Unsupported {
                call: "extension management".into(),
            }),
        }
    }

    fn terminal(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if matches!(request, Request::TerminalTabs | Request::TerminalTopology) {
            let port = self
                .peer
                .authority()
                .port(Capability::TerminalRead, services.terminal)?;
            return match request {
                Request::TerminalTabs => Ok(Reply::Tabs(port.tabs()?)),
                Request::TerminalTopology => Ok(Reply::Topology(port.topology()?)),
                _ => unreachable!(),
            };
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
                validate_terminal_command(command)?;
                port.spawn(slot, command).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalWritePane { slot, contents } => {
                if contents.len() > PANE_INPUT_BYTES {
                    return Err(Failure::Conflict {
                        detail: format!("terminal input exceeds the {PANE_INPUT_BYTES} byte limit"),
                    });
                }
                port.write(slot, contents).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalResizeGrid { slot, columns, rows } => {
                if *columns == 0 || *rows == 0 || *columns > PANE_GRID_EDGE || *rows > PANE_GRID_EDGE {
                    return Err(Failure::Conflict {
                        detail: format!("terminal grid must be within 1..={PANE_GRID_EDGE} rows and columns"),
                    });
                }
                port.resize_grid(
                    slot,
                    GridSize {
                        columns: *columns,
                        rows: *rows,
                    },
                )
                .map(|()| Reply::Done)
                .map_err(Failure::from)
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
        Ok(Reply::Text(crate::port::bounded_pane_text(
            port.read(slot, pane_lines(lines))?,
        )))
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
            Request::FilesystemStat { path } => {
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Entry(port.stat(path)?))
            }
            Request::FilesystemWrite { path, contents } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.write(path, contents).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::FilesystemMkdir { path } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.mkdir(path).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::FilesystemRename { from, to } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.rename(from, to).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::FilesystemRemove { path } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.remove(path).map(|()| Reply::Done).map_err(Failure::from)
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
    fn render(&mut self, slot: &str, frame: &hl_gui::Frame) -> Result<Reply, Failure> {
        if !self.surfaces.contains(slot) {
            return Err(Failure::Conflict {
                detail: format!("surface {slot} is not owned by this session"),
            });
        }
        self.pending.push(SurfaceFrame {
            slot: slot.to_owned(),
            frame: frame.clone(),
        });
        Ok(Reply::Done)
    }

    fn render_legacy(&mut self, frame: &hl_gui::Frame) -> Result<Reply, Failure> {
        let slot = self.only_surface()?;
        self.render(&slot, frame)
    }

    /// Accepts a change to a windowed source the session's tables draw from.
    fn mutate(&mut self, slot: &str, mutation: hl_gui::SourceMutation) -> Result<Reply, Failure> {
        if !self.surfaces.contains(slot) {
            return Err(Failure::Conflict {
                detail: format!("surface {slot} is not owned by this session"),
            });
        }
        self.mutations.push(SurfaceMutation {
            slot: slot.to_owned(),
            mutation,
        });
        Ok(Reply::Done)
    }

    fn mutate_legacy(&mut self, mutation: hl_gui::SourceMutation) -> Result<Reply, Failure> {
        let slot = self.only_surface()?;
        self.mutate(&slot, mutation)
    }

    fn only_surface(&self) -> Result<String, Failure> {
        if self.surfaces.len() != 1 {
            return Err(Failure::Conflict {
                detail: "an unaddressed interface call requires exactly one owned surface".into(),
            });
        }
        Ok(self.surfaces.iter().next().expect("one surface").clone())
    }

    /// Source changes received since the host last collected them.
    #[must_use]
    pub fn drain_sources(&mut self) -> Vec<SurfaceMutation> {
        std::mem::take(&mut self.mutations)
    }

    /// Interface frames received since the host last collected them.
    ///
    /// The protocol layer holds them rather than applying them: it owns no
    /// toolkit, and the surface belongs to the host.
    #[must_use]
    pub fn drain(&mut self) -> Vec<SurfaceFrame> {
        std::mem::take(&mut self.pending)
    }

    /// Opens and records one independently addressable interface surface.
    fn open_tab(&mut self, title: &str, services: &Services<'_>) -> Result<Reply, Failure> {
        const SURFACE_LIMIT: usize = 32;
        if self.surfaces.len() >= SURFACE_LIMIT {
            return Err(Failure::Conflict {
                detail: format!("interface surface limit of {SURFACE_LIMIT} is exhausted"),
            });
        }
        let port = self.peer.authority().port(Capability::Interface, services.terminal)?;
        let id = port.open_tab(title)?;
        self.surfaces.insert(id.clone());
        Ok(Reply::Identity(id))
    }

    /// Divides a pane and records the new independently addressable surface.
    fn open_pane(&mut self, slot: &str, division: Division, services: &Services<'_>) -> Result<Reply, Failure> {
        if self.surfaces.len() >= 32 {
            return Err(Failure::Conflict {
                detail: "interface surface limit of 32 is exhausted".into(),
            });
        }
        let port = self.peer.authority().port(Capability::Interface, services.terminal)?;
        let id = port.surface(slot, division)?;
        self.surfaces.insert(id.clone());
        Ok(Reply::Identity(id))
    }

    /// Retires one surface owned by this session without disturbing siblings.
    fn withdraw(&mut self, slot: &str, services: &Services<'_>) -> Result<Reply, Failure> {
        if !self.surfaces.contains(slot) {
            return Err(Failure::Conflict {
                detail: format!("surface {slot} is not owned by this session"),
            });
        }
        let port = self.peer.authority().port(Capability::Interface, services.terminal)?;
        port.close(slot)?;
        self.surfaces.remove(slot);
        self.pending.retain(|frame| frame.slot != slot);
        self.mutations.retain(|mutation| mutation.slot != slot);
        Ok(Reply::Done)
    }
}

fn bounded_signal(signal: &str) -> Result<(), Failure> {
    if !signal.is_empty() && signal.len() <= 32 {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: "signal must contain 1..=32 bytes".into(),
    })
}

fn immutable_identity(id: &str, widths: &[usize], noun: &str) -> Result<(), Failure> {
    if widths.contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: format!("{noun} signaling requires the complete immutable ID returned by inspection"),
    })
}

fn immutable_reference<'a>(id: &'a str, widths: &[usize], noun: &str) -> Result<&'a str, Failure> {
    immutable_identity(id, widths, noun)?;
    Ok(id)
}

fn immutable_digest(value: &str, noun: &str) -> Result<(), Failure> {
    let digest = value.strip_prefix("sha256:").unwrap_or_default();
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: format!("{noun} removal requires the complete immutable sha256 digest returned by inventory"),
    })
}

fn validate_terminal_command(command: &[String]) -> Result<(), Failure> {
    if !command.is_empty()
        && command.len() <= crate::port::TERMINAL_COMMAND_ARGUMENTS
        && !command[0].is_empty()
        && command
            .iter()
            .all(|argument| argument.len() <= crate::port::TERMINAL_COMMAND_ARGUMENT_BYTES && !argument.contains('\0'))
        && command.iter().map(String::len).sum::<usize>() <= crate::port::TERMINAL_COMMAND_BYTES
    {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: "terminal command must contain 1..=64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in aggregate".into(),
    })
}

fn validate_container_create(spec: &crate::port::ContainerCreateSpec) -> Result<(), Failure> {
    use std::collections::BTreeSet;

    let identifier = |value: &str, limit: usize| {
        !value.is_empty()
            && value.len() <= limit
            && value.trim() == value
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    };
    let argv = |values: &[String], empty: bool| {
        (empty || !values.is_empty())
            && values.len() <= crate::port::TERMINAL_COMMAND_ARGUMENTS
            && (values.is_empty() || !values[0].is_empty())
            && values
                .iter()
                .all(|value| value.len() <= crate::port::TERMINAL_COMMAND_ARGUMENT_BYTES && !value.contains('\0'))
            && values.iter().map(String::len).sum::<usize>() <= crate::port::TERMINAL_COMMAND_BYTES
    };
    let absolute = |value: &str| {
        value.starts_with('/')
            && value.len() <= 4096
            && !value.contains('\0')
            && !value.split('/').any(|part| matches!(part, "." | ".."))
    };
    let unique =
        |values: &[(String, String)]| values.iter().map(|(key, _)| key).collect::<BTreeSet<_>>().len() == values.len();
    let environment_name = |value: &str| {
        value.len() <= 256
            && value
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    let valid = identifier(&spec.name, 128)
        && !spec.image.is_empty()
        && spec.image.len() <= 512
        && spec.image.trim() == spec.image
        && !spec.image.chars().any(char::is_whitespace)
        && spec.entrypoint.as_ref().is_none_or(|values| argv(values, false))
        && argv(&spec.command, true)
        && spec
            .entrypoint
            .as_ref()
            .into_iter()
            .flatten()
            .chain(spec.command.iter())
            .map(String::len)
            .sum::<usize>()
            <= crate::port::TERMINAL_COMMAND_BYTES
        && spec.environment.len() <= 256
        && unique(&spec.environment)
        && spec
            .environment
            .iter()
            .all(|(name, value)| environment_name(name) && value.len() <= 8192 && !value.contains('\0'))
        && spec.working_directory.as_deref().is_none_or(absolute)
        && spec
            .user
            .as_ref()
            .is_none_or(|value| !value.is_empty() && value.len() <= 256 && !value.contains('\0'))
        && spec.labels.len() <= 128
        && unique(&spec.labels)
        && spec.labels.iter().all(|(name, value)| {
            !name.is_empty()
                && name.len() <= 256
                && !name.contains('\0')
                && value.len() <= 4096
                && !value.contains('\0')
        })
        && spec.mounts.len() <= 64
        && spec
            .mounts
            .iter()
            .all(|mount| identifier(&mount.volume, 128) && absolute(&mount.target))
        && spec.network.as_ref().is_none_or(|network| identifier(network, 256))
        && spec.ports.len() <= 64
        && spec
            .ports
            .iter()
            .all(|port| port.container != 0 && port.host != Some(0) && matches!(port.protocol.as_str(), "tcp" | "udp"))
        && spec
            .ports
            .iter()
            .map(|port| (port.container, &port.protocol))
            .collect::<BTreeSet<_>>()
            .len()
            == spec.ports.len()
        && spec.memory_mb.is_none_or(|value| (1..=1_048_576).contains(&value))
        && spec.cpus.is_none_or(|value| (1..=256).contains(&value))
        && spec.pids_limit.is_none_or(|value| (1..=1_000_000).contains(&value));
    if valid {
        Ok(())
    } else {
        Err(Failure::Conflict {
            detail: "container creation specification is invalid or exceeds its bound".into(),
        })
    }
}

fn acquisition_reference(reference: &str) -> Result<(), Failure> {
    if !reference.is_empty()
        && reference.len() <= crate::port::EXTENSION_REFERENCE_BYTES
        && reference.trim() == reference
        && !reference.chars().any(char::is_whitespace)
    {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: "extension image reference must contain 1..=512 bytes without whitespace".into(),
    })
}

fn acquisition_job(job: &str) -> Result<(), Failure> {
    if !job.is_empty() && job.len() <= crate::port::EXTENSION_JOB_BYTES {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: "extension acquisition job must contain 1..=128 bytes".into(),
    })
}
