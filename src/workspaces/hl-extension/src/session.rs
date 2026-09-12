//! One connected extension: what it may reach, and what it is subscribed to.
//!
//! The bookkeeping — the authority, the topics followed, revocation — is
//! `hl-rpc`'s [`hl_rpc::Session`]. What a call means is this domain's, and lives
//! here: [`Session::dispatch`] is the one place a workspace call reaches a
//! workspace service, always through a capability check.

use hl_rpc::Authority;

use crate::capability::Capability;
use crate::port::{
    ContainerControl, ContainerInventory, Division, ExtensionStateStore, ExtensionStore, GridSize, ImageStore,
    NetworkStore, NotificationSink, PANE_GRID_EDGE, PANE_INPUT_BYTES, TerminalSurface, VolumeStore,
    WorkspaceConfiguration, WorkspaceControl, WorkspaceFiles, WorkspaceInventory, pane_lines,
};
use crate::request::{Failure, Reply, Request, Topic, WorkspaceInfo};
use crate::{ContainerGrant, ContainerSelector, FilesystemGrant};

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
    pub state: &'a dyn ExtensionStateStore,
    pub notifications: &'a dyn NotificationSink,
}

/// One connected extension.
pub struct Session {
    peer: hl_rpc::Session<Topic>,
    surfaces: std::collections::BTreeSet<String>,
    pending: Vec<SurfaceFrame>,
    mutations: Vec<SurfaceMutation>,
    containers: ContainerGrant,
    networks: crate::NetworkGrant,
    volumes: crate::VolumeGrant,
    images: crate::ImageGrant,
    extension_identity: String,
    filesystem: FilesystemGrant,
    workspace_environment: crate::WorkspaceEnvironmentGrant,
    notification_ids: std::collections::BTreeSet<String>,
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

fn workspace_environment_patch(patch: &crate::port::WorkspaceEnvironmentPatch) -> Result<(), Failure> {
    let names = patch
        .set
        .iter()
        .map(|(name, _)| name)
        .chain(patch.remove.iter())
        .collect::<Vec<_>>();
    let bytes = patch
        .set
        .iter()
        .try_fold(0usize, |total, (name, value)| {
            total.checked_add(name.len() + value.len())
        })
        .and_then(|total| {
            patch
                .remove
                .iter()
                .try_fold(total, |sum, name| sum.checked_add(name.len()))
        })
        .ok_or_else(|| Failure::Conflict {
            detail: "workspace environment patch is too large".into(),
        })?;
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 256
            && name
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit())
    };
    if names.len() > 128
        || bytes > 32 * 1024
        || patch
            .set
            .iter()
            .any(|(_, value)| value.len() > 4096 || value.contains('\0'))
        || names.iter().any(|name| !valid_name(name))
        || names.iter().collect::<std::collections::BTreeSet<_>>().len() != names.len()
    {
        return Err(Failure::Conflict {
            detail: "workspace environment patch must contain at most 128 unique, disjoint, bounded names and values"
                .into(),
        });
    }
    Ok(())
}

const PREFERENCE_ENTRIES: usize = 64;
const PREFERENCE_KEY_BYTES: usize = 64;
const PREFERENCE_STRING_BYTES: usize = 1024;
const CREDENTIAL_VALUE_BYTES: usize = 64 * 1024;

fn validate_credential_key(key: &str) -> Result<(), Failure> {
    if key.is_empty()
        || key.len() > PREFERENCE_KEY_BYTES
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Failure::Conflict {
            detail: "credential keys must be 1 through 64 ASCII letters, digits, '.', '_' or '-'".into(),
        });
    }
    Ok(())
}

fn validate_preference_key(key: &str) -> Result<(), Failure> {
    if key.is_empty()
        || key.len() > PREFERENCE_KEY_BYTES
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Failure::Conflict {
            detail: "preference keys must be 1 through 64 ASCII letters, digits, '.', '_' or '-'".into(),
        });
    }
    Ok(())
}

fn validate_preference(key: &str, value: &crate::port::PreferenceValue) -> Result<(), Failure> {
    validate_preference_key(key)?;
    match value {
        crate::port::PreferenceValue::Number(value) if value.unsigned_abs() > crate::JSON_SAFE_INTEGER_MAX => {
            Err(Failure::Conflict {
                detail: "preference numbers must be JavaScript-safe integers".into(),
            })
        }
        crate::port::PreferenceValue::String(value) if value.len() > PREFERENCE_STRING_BYTES => {
            Err(Failure::Conflict {
                detail: "preference strings are limited to 1024 bytes".into(),
            })
        }
        _ => Ok(()),
    }
}

fn validate_preferences(preferences: &crate::port::ExtensionPreferences) -> Result<(), Failure> {
    if preferences.entries.len() > PREFERENCE_ENTRIES {
        return Err(Failure::Failed {
            detail: "host returned more than 64 preferences".into(),
        });
    }
    let mut keys = std::collections::BTreeSet::new();
    for (key, value) in &preferences.entries {
        validate_preference(key, value).map_err(|_| Failure::Failed {
            detail: "host returned invalid extension preferences".into(),
        })?;
        if !keys.insert(key) {
            return Err(Failure::Failed {
                detail: "host returned duplicate preference keys".into(),
            });
        }
    }
    Ok(())
}

fn terminal_command_summary(
    execution: crate::port::ExecutionSummary,
    slot: &str,
    generation: u64,
    revision: u64,
) -> crate::port::TerminalCommand {
    crate::port::TerminalCommand {
        id: execution.id,
        slot: slot.to_owned(),
        generation,
        revision,
        running: execution.running,
        exit_code: execution.exit_code,
        pid: execution.pid,
        command: execution.command,
    }
}

fn validate_execution_output(page: &crate::port::ExecutionOutputPage, after: u64, limit: u16) -> Result<(), Failure> {
    let ordered = page
        .entries
        .iter()
        .try_fold(after, |previous, entry| {
            (entry.sequence > previous).then_some(entry.sequence)
        })
        .is_some();
    let contiguous = page
        .entries
        .windows(2)
        .all(|pair| pair[1].sequence == pair[0].sequence.saturating_add(1));
    let gap = page
        .entries
        .first()
        .is_some_and(|entry| entry.sequence != after.saturating_add(1));
    let bytes = page
        .entries
        .iter()
        .fold(0usize, |total, entry| total.saturating_add(entry.bytes.len()));
    if page.entries.len() > usize::from(limit)
        || (page.eof && page.more)
        || bytes > 256 * 1024
        || !ordered
        || !contiguous
        || page.gap != gap
        || page.next != page.entries.last().map_or(after, |entry| entry.sequence)
        || page
            .entries
            .iter()
            .any(|entry| !matches!(entry.stream.as_str(), "stdout" | "stderr"))
    {
        return Err(Failure::Failed {
            detail: "host returned invalid or oversized execution output page".into(),
        });
    }
    Ok(())
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
            containers: ContainerGrant::default(),
            networks: crate::NetworkGrant::default(),
            volumes: crate::VolumeGrant::default(),
            images: crate::ImageGrant::default(),
            extension_identity: String::new(),
            filesystem: FilesystemGrant::default(),
            workspace_environment: crate::WorkspaceEnvironmentGrant::default(),
            notification_ids: std::collections::BTreeSet::new(),
        }
    }

    /// Applies the exact resource consent recorded for this connection.
    #[must_use]
    pub fn with_containers(mut self, containers: ContainerGrant) -> Self {
        self.containers = containers;
        self
    }

    #[must_use]
    pub fn with_networks(mut self, networks: crate::NetworkGrant) -> Self {
        self.networks = networks;
        self
    }

    #[must_use]
    pub fn with_volumes(mut self, volumes: crate::VolumeGrant) -> Self {
        self.volumes = volumes;
        self
    }

    #[must_use]
    pub fn with_images(mut self, images: crate::ImageGrant) -> Self {
        self.images = images;
        self
    }

    #[must_use]
    pub fn with_extension_identity(mut self, identity: impl Into<String>) -> Self {
        self.extension_identity = identity.into();
        self
    }

    #[must_use]
    pub fn extension_identity(&self) -> &str {
        &self.extension_identity
    }

    #[must_use]
    pub fn visible_images(&self, images: Vec<crate::port::ImageSummary>) -> crate::port::ImageInventory {
        let images = images
            .into_iter()
            .filter_map(|mut image| {
                image.references.retain(|reference| self.images.permits_read(reference));
                let digest_visible = self.images.permits_read(&image.id);
                if image.references.is_empty() && !digest_visible {
                    return None;
                }
                image.reference = image.references.first().cloned().unwrap_or_else(|| image.id.clone());
                Some(image)
            })
            .collect();
        crate::port::ImageInventory::bounded(images)
    }

    fn canonical_image(reference: &str) -> Result<String, Failure> {
        if reference.len() == 71
            && reference.starts_with("sha256:")
            && reference[7..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Ok(reference.to_owned());
        }
        let canonical = reference
            .parse::<hl_image_reference::ImageReference>()
            .map_err(|error| Failure::Conflict {
                detail: format!("invalid image reference: {error}"),
            })?
            .to_string();
        Ok(canonical)
    }

    fn permit_image(&self, reference: &str, capability: Capability) -> Result<String, Failure> {
        let canonical = Self::canonical_image(reference)?;
        let permitted = match capability {
            Capability::ImageRead => self.images.permits_read(&canonical),
            Capability::ImagePull => self.images.permits_pull(&canonical),
            Capability::ImageRemove => self.images.permits_remove(&canonical),
            _ => false,
        };
        if !permitted {
            Err(Failure::Denied {
                capability: capability.as_str().into(),
                detail: "image is outside the extension's consented resource scope".into(),
            })
        } else {
            Ok(canonical)
        }
    }

    fn permit_image_use(&self, reference: &str) -> Result<(), Failure> {
        let canonical = Self::canonical_image(reference)?;
        if self.images.permits_use(&canonical) {
            Ok(())
        } else {
            Err(Failure::Denied {
                capability: Capability::ContainerCreate.as_str().into(),
                detail: "container image use is outside the extension's consented image scope".into(),
            })
        }
    }

    #[must_use]
    pub fn with_filesystem(mut self, filesystem: FilesystemGrant) -> Self {
        self.filesystem = filesystem;
        self
    }

    #[must_use]
    pub fn with_workspace_environment(mut self, grant: crate::WorkspaceEnvironmentGrant) -> Self {
        self.workspace_environment = grant;
        self
    }

    #[must_use]
    pub fn filesystem_read_selectors(&self) -> &[crate::FilesystemSelector] {
        &self.filesystem.read
    }

    /// The caller's effective filesystem scope, for handshake disclosure.
    #[must_use]
    pub const fn filesystem_grant(&self) -> &FilesystemGrant {
        &self.filesystem
    }

    #[must_use]
    pub const fn container_grant(&self) -> &ContainerGrant {
        &self.containers
    }
    #[must_use]
    pub const fn image_grant(&self) -> &crate::ImageGrant {
        &self.images
    }
    #[must_use]
    pub const fn network_grant(&self) -> &crate::NetworkGrant {
        &self.networks
    }
    #[must_use]
    pub const fn volume_grant(&self) -> &crate::VolumeGrant {
        &self.volumes
    }
    #[must_use]
    pub const fn workspace_environment_grant(&self) -> &crate::WorkspaceEnvironmentGrant {
        &self.workspace_environment
    }

    #[must_use]
    pub fn visible_networks(&self, networks: Vec<crate::port::NetworkSummary>) -> crate::port::NetworkInventory {
        crate::port::NetworkInventory::bounded(
            networks
                .into_iter()
                .filter(|network| self.networks.permits(&network.id, &network.name))
                .collect(),
        )
    }

    #[must_use]
    pub fn visible_volumes(&self, volumes: Vec<crate::port::VolumeSummary>) -> crate::port::VolumeInventory {
        crate::port::VolumeInventory::bounded(
            volumes
                .into_iter()
                .filter(|volume| self.volumes.permits(&volume.name))
                .collect(),
        )
    }

    fn permit_filesystem_path(
        &self,
        capability: Capability,
        access: FilesystemAccess,
        path: &hl_rpc::RelativePath,
    ) -> Result<(), Failure> {
        self.peer.authority().permit(capability)?;
        let roots: &[crate::FilesystemSelector] = match capability {
            Capability::FilesystemRead => &self.filesystem.read,
            Capability::FilesystemWrite => match access {
                FilesystemAccess::Write => &self.filesystem.write,
                FilesystemAccess::Create => &self.filesystem.create,
                FilesystemAccess::Delete => &self.filesystem.delete,
                FilesystemAccess::Rename => &self.filesystem.rename,
                FilesystemAccess::List | FilesystemAccess::Read => &[],
            },
            _ => {
                return Err(Failure::Denied {
                    capability: capability.as_str().into(),
                    detail: "non-filesystem capability used for path authority".into(),
                });
            }
        };
        if roots.iter().any(|selector| {
            selector.permits(path)
                && (!matches!(access, FilesystemAccess::List)
                    || matches!(selector, crate::FilesystemSelector::Subtree { .. }))
        }) {
            return Ok(());
        }
        Err(Failure::Denied {
            capability: capability.as_str().into(),
            detail: "path is outside the extension's consented resource scope".into(),
        })
    }

    fn visible_container(&self, container: &crate::port::ContainerSummary) -> bool {
        self.containers.all()
            || self.containers.selectors.iter().any(|selector| match selector {
                ContainerSelector::Id { id } => container.id == *id,
                ContainerSelector::Name { name } => container.name == *name,
                ContainerSelector::All { all } => *all,
            })
    }

    /// Filters a host inventory before it crosses the extension boundary.
    #[must_use]
    pub fn visible_containers(
        &self,
        containers: Vec<crate::port::ContainerSummary>,
    ) -> Vec<crate::port::ContainerSummary> {
        containers
            .into_iter()
            .filter(|container| self.visible_container(container))
            .collect()
    }

    /// Filters executions by the immutable identities of currently visible containers.
    #[must_use]
    pub fn visible_executions(
        &self,
        mut executions: crate::port::ExecutionList,
        containers: &[crate::port::ContainerSummary],
    ) -> crate::port::ExecutionList {
        let ids: std::collections::BTreeSet<&str> = containers
            .iter()
            .filter(|container| self.visible_container(container))
            .map(|container| container.id.as_str())
            .collect();
        executions
            .executions
            .retain(|execution| ids.contains(execution.container_id.as_str()));
        executions
    }

    fn resolve_container(
        &self,
        id: &str,
        port: &dyn ContainerInventory,
    ) -> Result<crate::port::ContainerSummary, Failure> {
        self.resolve_container_for(id, port, Capability::ContainerRead)
    }

    fn resolve_container_for(
        &self,
        id: &str,
        port: &dyn ContainerInventory,
        capability: Capability,
    ) -> Result<crate::port::ContainerSummary, Failure> {
        if let Some(container) = port
            .list()?
            .into_iter()
            .find(|container| container.id == id && self.visible_container(container))
        {
            return Ok(container);
        }
        if self.containers.all()
            || self
                .containers
                .selectors
                .iter()
                .any(|selector| matches!(selector, ContainerSelector::Id { id: allowed } if allowed == id))
        {
            return Ok(crate::port::ContainerSummary {
                id: id.to_owned(),
                name: String::new(),
                image: String::new(),
                state: String::new(),
                created: 0,
                generation: 0,
                ports: Vec::new(),
            });
        }
        Err(Failure::Denied {
            capability: capability.as_str().into(),
            detail: "container is outside the extension's consented resource scope".into(),
        })
    }

    fn resolve_mutation_container(
        &self,
        id: &str,
        port: &dyn ContainerInventory,
    ) -> Result<crate::port::ContainerSummary, Failure> {
        immutable_identity(id, &[32, 64], "container")?;
        let container = self.resolve_container(id, port)?;
        Ok(container)
    }

    fn resolve_execution(
        &self,
        id: &str,
        port: &dyn ContainerInventory,
    ) -> Result<crate::port::ExecutionSummary, Failure> {
        immutable_identity(id, &[32], "execution")?;
        let execution = port.execution(id)?;
        self.resolve_container(&execution.container_id, port)?;
        Ok(execution)
    }

    /// Adds a surface the host already owns, such as the workspace overview.
    #[must_use]
    pub fn with_surface(mut self, slot: impl Into<String>) -> Self {
        self.surfaces.insert(slot.into());
        self
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
            Request::FilesystemReadRanges { ranges } => {
                self.peer.authority().permit(capability)?;
                for range in ranges {
                    self.permit_filesystem_path(capability, FilesystemAccess::Read, &range.path)?;
                }
            }
            Request::FilesystemRename { from, to } | Request::FilesystemRenameObserved { from, to, .. } => {
                self.permit_filesystem_path(capability, FilesystemAccess::Rename, from)?;
                self.permit_filesystem_path(capability, FilesystemAccess::Rename, to)?;
            }
            _ => match request.path() {
                Some(path) => self.permit_filesystem_path(capability, filesystem_access(request), path)?,
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
            | Request::WorkspaceUpdate { .. }
            | Request::WorkspaceEnvironmentPatch { .. }
            | Request::WorkspaceDelete { .. }
            | Request::WorkspaceStart { .. }
            | Request::WorkspaceStop { .. }
            | Request::WorkspaceRestart { .. } => self.workspace_control(request, services),
            Request::ExtensionList
            | Request::ExtensionCatalogue
            | Request::ExtensionInspect { .. }
            | Request::ExtensionEnable { .. }
            | Request::ExtensionDisable { .. }
            | Request::ExtensionRetry { .. }
            | Request::ExtensionRemove { .. }
            | Request::ExtensionAcquisitionStart { .. }
            | Request::ExtensionAcquisitionStatus { .. }
            | Request::ExtensionAcquisitionCancel { .. }
            | Request::ExtensionInstall { .. }
            | Request::ExtensionUpdate { .. } => self.extensions(request, services),
            Request::NotificationPublish { notification } => {
                validate_notification(notification)?;
                if !self.notification_ids.contains(&notification.id) && self.notification_ids.len() >= 32 {
                    return Err(Failure::Conflict {
                        detail: "one extension session may publish at most 32 notification identities".into(),
                    });
                }
                services.notifications.publish(notification)?;
                self.notification_ids.insert(notification.id.clone());
                Ok(Reply::Done)
            }
            Request::ContainerList
            | Request::ContainerInspect { .. }
            | Request::ContainerInspectObserved { .. }
            | Request::ContainerProcesses { .. }
            | Request::ContainerLogs { .. }
            | Request::ExecutionInspect { .. }
            | Request::ExecutionList
            | Request::ExecutionLogs { .. }
            | Request::ExecutionOutput { .. }
            | Request::ExecutionWait { .. } => self.containers(request, services),
            Request::ContainerAttachTerminal { id, command } => {
                immutable_identity(id, &[32, 64], "container")?;
                let target = self.resolve_mutation_container(id, services.containers)?;
                validate_terminal_command(command)?;
                let port = self
                    .peer
                    .authority()
                    .port(Capability::ContainerAttach, services.terminal)?;
                Ok(Reply::Identity(port.attach_container(
                    &target.id,
                    target.generation,
                    command,
                )?))
            }
            Request::ContainerCreate { .. }
            | Request::ContainerStart { .. }
            | Request::ContainerStop { .. }
            | Request::ContainerRemove { .. }
            | Request::ContainerPause { .. }
            | Request::ContainerUnpause { .. }
            | Request::ContainerRestart { .. }
            | Request::ContainerRename { .. }
            | Request::ContainerKill { .. }
            | Request::ExecutionKill { .. }
            | Request::ExecutionCancel { .. }
            | Request::ExecutionRemove { .. }
            | Request::ExecutionWrite { .. }
            | Request::ExecutionCloseInput { .. }
            | Request::ContainerExec { .. }
            | Request::ContainerExecCredential { .. } => self.control(request, services),
            Request::ImageList
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
            | Request::TerminalPinTab { .. }
            | Request::TerminalSplit { .. }
            | Request::TerminalSplitObserved { .. }
            | Request::TerminalSpawn { .. }
            | Request::TerminalSpawnObserved { .. }
            | Request::TerminalCommandStart { .. }
            | Request::TerminalCommandInspect { .. }
            | Request::TerminalCommandOutput { .. }
            | Request::TerminalCommandWait { .. }
            | Request::TerminalCommandCancel { .. }
            | Request::TerminalCommandWrite { .. }
            | Request::TerminalCommandCloseInput { .. }
            | Request::TerminalReadPane { .. }
            | Request::TerminalWritePane { .. }
            | Request::TerminalResizeGrid { .. }
            | Request::TerminalResizeGridObserved { .. }
            | Request::TerminalClosePane { .. }
            | Request::TerminalClosePaneObserved { .. }
            | Request::TerminalFocusPane { .. }
            | Request::TerminalFocusPaneObserved { .. }
            | Request::TerminalRetitlePane { .. }
            | Request::TerminalRetitlePaneObserved { .. }
            | Request::TerminalRatio { .. }
            | Request::TerminalRatioObserved { .. }
            | Request::TerminalSwitchOccupant { .. }
            | Request::TerminalSwitchOccupantObserved { .. } => self.terminal(request, services),
            Request::PaneList => {
                let port = self.peer.authority().port(Capability::PaneObserve, services.terminal)?;
                Ok(Reply::Panes(port.pane_inventory()?))
            }
            Request::PaneSemanticRead { slot } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::PaneSemanticRead, services.terminal)?;
                let tree = port.semantics(slot)?;
                validate_semantic_tree(slot, &tree)?;
                Ok(Reply::Semantics(tree))
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
            Request::FilesystemInventory
            | Request::FilesystemChanges { .. }
            | Request::FilesystemList { .. }
            | Request::FilesystemListPage { .. }
            | Request::FilesystemRead { .. }
            | Request::FilesystemReadRange { .. }
            | Request::FilesystemReadRanges { .. }
            | Request::FilesystemStat { .. }
            | Request::FilesystemWrite { .. }
            | Request::FilesystemWriteObserved { .. }
            | Request::FilesystemCreateObserved { .. }
            | Request::FilesystemMkdir { .. }
            | Request::FilesystemRename { .. }
            | Request::FilesystemRenameObserved { .. }
            | Request::FilesystemRemove { .. }
            | Request::FilesystemRemoveObserved { .. } => self.files(request, services),
            Request::StateRead
            | Request::StateWrite { .. }
            | Request::StateClear { .. }
            | Request::PreferenceRead
            | Request::PreferenceSet { .. }
            | Request::PreferenceRemove { .. }
            | Request::CredentialRead { .. }
            | Request::CredentialSet { .. }
            | Request::CredentialRemove { .. } => self.state(request, services),
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
            Request::ContainerInspect { id } => {
                let target = self.resolve_container(id, port.port())?;
                Ok(Reply::Container(port.inspect(&target.id)?))
            }
            Request::ContainerInspectObserved { id, generation } => {
                let target = self.resolve_container(id, port.port())?;
                let inspected = port.inspect(&target.id)?;
                if inspected.generation != *generation {
                    return Err(Failure::Conflict {
                        detail: format!(
                            "container {id} generation changed from {generation} to {}",
                            inspected.generation
                        ),
                    });
                }
                Ok(Reply::Container(inspected))
            }
            Request::ContainerProcesses {
                id,
                snapshot,
                after,
                limit,
            } => {
                if !(1..=128).contains(limit) {
                    return Err(Failure::Conflict {
                        detail: "container process page limit must be between 1 and 128".into(),
                    });
                }
                if snapshot
                    .as_ref()
                    .is_some_and(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
                {
                    return Err(Failure::Conflict {
                        detail: "container process snapshot must be 64 hexadecimal characters".into(),
                    });
                }
                if *after > 0 && snapshot.is_none() {
                    return Err(Failure::Conflict {
                        detail: "container process continuation requires its snapshot identity".into(),
                    });
                }
                let target = self.resolve_container(id, port.port())?;
                Ok(Reply::Processes(port.processes(
                    &target.id,
                    snapshot.as_deref(),
                    *after,
                    *limit,
                )?))
            }
            Request::ContainerLogs { id, stdout, stderr } => {
                let target = self.resolve_container(id, port.port())?;
                Ok(Reply::Logs(port.logs(&target.id, *stdout, *stderr)?))
            }
            Request::ExecutionInspect { id } => Ok(Reply::Execution(self.resolve_execution(id, port.port())?)),
            Request::ExecutionList => {
                let containers = port.list()?;
                Ok(Reply::Executions(
                    self.visible_executions(port.executions()?, &containers),
                ))
            }
            Request::ExecutionLogs { id, stdout, stderr } => {
                immutable_identity(id, &[32], "execution")?;
                self.resolve_execution(id, port.port())?;
                if !stdout && !stderr {
                    return Err(Failure::Conflict {
                        detail: "execution logs require stdout or stderr".into(),
                    });
                }
                Ok(Reply::Logs(port.execution_logs(id, *stdout, *stderr)?))
            }
            Request::ExecutionOutput { id, after, limit } => {
                immutable_identity(id, &[32], "execution")?;
                if *limit == 0 || *limit > 16 {
                    return Err(Failure::Conflict {
                        detail: "execution output limit must be between 1 and 16".into(),
                    });
                }
                self.resolve_execution(id, port.port())?;
                let page = port.execution_output(id, *after, *limit)?;
                validate_execution_output(&page, *after, *limit)?;
                Ok(Reply::ExecutionOutput(page))
            }
            Request::ExecutionWait { id, timeout_ms } => {
                immutable_identity(id, &[32], "execution")?;
                self.resolve_execution(id, port.port())?;
                if !(1..=30_000).contains(timeout_ms) {
                    return Err(Failure::Conflict {
                        detail: "execution wait timeout_ms must be between 1 and 30000".into(),
                    });
                }
                Ok(Reply::Execution(port.execution_wait(id, *timeout_ms)?))
            }
            Request::ContainerList => Ok(Reply::Containers(self.visible_containers(port.list()?))),
            _ => Err(Failure::Unsupported {
                call: "container read".into(),
            }),
        }
    }

    fn control(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if let Request::ContainerCreate { spec } = request {
            validate_container_create(spec)?;
            if !self.containers.create {
                return Err(Failure::Denied {
                    capability: Capability::ContainerCreate.as_str().into(),
                    detail: "container creation was not consented".into(),
                });
            }
            self.permit_image_use(&spec.image)?;
            if spec.mounts.iter().any(|mount| mount.read_only) {
                self.peer.authority().permit(Capability::VolumeRead)?;
            }
            if spec.mounts.iter().any(|mount| !mount.read_only) {
                self.peer.authority().permit(Capability::VolumeWrite)?;
            }
            for mount in &spec.mounts {
                self.permit_volume(
                    &mount.volume,
                    if mount.read_only {
                        Capability::VolumeRead
                    } else {
                        Capability::VolumeWrite
                    },
                )?;
            }
            if spec.network.is_some() || spec.ports.iter().any(|port| port.host.is_some()) {
                self.peer.authority().permit(Capability::NetworkWrite)?;
            }
            if let Some(network) = &spec.network {
                self.permit_network_reference(network, Capability::NetworkWrite)?;
            }
        }
        let port = self.peer.authority().port(request.capability(), services.control)?;
        match request {
            Request::ContainerCreate { spec } => {
                validate_container_create(spec)?;
                Ok(Reply::Identity(port.create_spec(spec)?))
            }
            Request::ContainerStart { id, generation } => {
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.start(id, &target.id, *generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerStop { id, generation } => {
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.stop(id, &target.id, *generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerRemove { id, generation } => {
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.remove(id, &target.id, *generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerPause { id, generation } => {
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.pause(id, &target.id, *generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerUnpause { id, generation } => {
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.unpause(id, &target.id, *generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerRestart { id, generation } => {
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.restart(id, &target.id, *generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerRename { id, generation, name } => {
                validate_container_name(name)?;
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.rename(id, &target.id, *generation, name)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerKill { id, generation, signal } => {
                bounded_signal(signal)?;
                let target = self.resolve_mutation_container(id, services.containers)?;
                port.kill(id, &target.id, *generation, signal)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExecutionKill { id, signal } => {
                bounded_signal(signal)?;
                self.resolve_execution(id, services.containers)?;
                port.execution_kill(id, signal)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExecutionCancel { id, signal, timeout_ms } => {
                bounded_signal(signal)?;
                if !(1..=30_000).contains(timeout_ms) {
                    return Err(Failure::Conflict {
                        detail: "execution cancellation timeout_ms must be between 1 and 30000".into(),
                    });
                }
                self.resolve_execution(id, services.containers)?;
                port.execution_cancel(id, signal, *timeout_ms)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExecutionRemove { id } => {
                self.resolve_execution(id, services.containers)?;
                port.execution_remove(id).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::ExecutionWrite { id, contents } => {
                immutable_identity(id, &[32], "execution")?;
                if contents.is_empty() || contents.len() > 64 * 1024 {
                    return Err(Failure::Conflict {
                        detail: "execution stdin chunks must contain between 1 and 65536 bytes".into(),
                    });
                }
                let execution = self.resolve_execution(id, services.containers)?;
                if !execution.running {
                    return Err(Failure::Conflict {
                        detail: "execution stdin is only writable while the execution is running".into(),
                    });
                }
                port.execution_write(id, contents)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExecutionCloseInput { id } => {
                immutable_identity(id, &[32], "execution")?;
                let execution = self.resolve_execution(id, services.containers)?;
                if !execution.running {
                    return Err(Failure::Conflict {
                        detail: "execution stdin can only be closed while the execution is running".into(),
                    });
                }
                port.execution_close_input(id)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ContainerExec {
                id,
                generation,
                command,
                environment,
                user,
                working_directory,
                stdin,
            } => {
                if *stdin {
                    self.peer.authority().permit(Capability::ContainerInput)?;
                }
                validate_exec_environment(environment)?;
                let target = self.resolve_mutation_container(id, services.containers)?;
                Ok(Reply::Identity(port.execute(
                    id,
                    &target.id,
                    *generation,
                    command,
                    environment,
                    user.as_deref(),
                    working_directory.as_deref(),
                    *stdin,
                )?))
            }
            Request::ContainerExecCredential {
                id,
                generation,
                command,
                environment,
                credentials,
                user,
                working_directory,
                stdin,
            } => {
                if *stdin {
                    self.peer.authority().permit(Capability::ContainerInput)?;
                }
                if credentials.len() > 64 {
                    return Err(Failure::Conflict {
                        detail: "credential environment is limited to 64 entries".into(),
                    });
                }
                let credentials_port = self
                    .peer
                    .authority()
                    .port(Capability::CredentialInject, services.state)?;
                let target = self.resolve_mutation_container(id, services.containers)?;
                let mut shape = environment.clone();
                for (variable, key) in credentials {
                    validate_credential_key(key)?;
                    shape.push((variable.clone(), crate::ExecEnvironmentValue::new("")));
                }
                validate_exec_environment(&shape)?;
                let mut resolved = environment.clone();
                for (variable, key) in credentials {
                    let value = credentials_port.credential(key)?.value.ok_or_else(|| Failure::Absent {
                        detail: format!("credential {key} is absent"),
                    })?;
                    let value = String::from_utf8(value).map_err(|_| Failure::Conflict {
                        detail: format!("credential {key} is not UTF-8 and cannot be an environment value"),
                    })?;
                    resolved.push((variable.clone(), crate::ExecEnvironmentValue::new(value)));
                }
                validate_exec_environment(&resolved)?;
                Ok(Reply::Identity(port.execute(
                    id,
                    &target.id,
                    *generation,
                    command,
                    &resolved,
                    user.as_deref(),
                    working_directory.as_deref(),
                    *stdin,
                )?))
            }
            _ => Err(Failure::Unsupported {
                call: "container control".into(),
            }),
        }
    }

    fn images(&mut self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self.peer.authority().port(request.capability(), services.images)?;
        match request {
            Request::ImageList => Ok(Reply::Images(self.visible_images(port.list()?))),
            Request::ImagePullStart { reference } => {
                let reference = self.permit_image(reference, Capability::ImagePull)?;
                let job = port.pull_start(&self.extension_identity, &reference)?;
                Ok(Reply::ImagePullJob(job))
            }
            Request::ImagePullStatus { job } => Ok(Reply::ImagePull(port.pull_status(&self.extension_identity, job)?)),
            Request::ImagePullCancel { job } => port
                .pull_cancel(&self.extension_identity, job)
                .map(|()| Reply::Done)
                .map_err(Failure::from),
            Request::ImageInspect { reference } => {
                let reference = self.permit_image(reference, Capability::ImageRead)?;
                let mut details = port.inspect(&reference)?;
                details
                    .references
                    .retain(|reference| self.images.permits_read(reference));
                Ok(Reply::ImageDetails(details))
            }
            Request::ImageRemove { reference } => {
                immutable_digest(reference, "image")?;
                self.permit_image(reference, Capability::ImageRemove)?;
                port.remove(reference).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::ImagePrune if self.images.prune_all_unused => Ok(Reply::ImagePrune(port.prune()?)),
            Request::ImagePrune => Err(Failure::Denied {
                capability: Capability::ImagePrune.as_str().into(),
                detail: "global image pruning was not consented".into(),
            }),
            _ => Err(Failure::Unsupported {
                call: "image operation".into(),
            }),
        }
    }

    fn volumes(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let capability = request.capability();
        let port = self.peer.authority().port(capability, services.volumes)?;
        match request {
            Request::VolumeList => Ok(Reply::Volumes(self.visible_volumes(port.list()?))),
            Request::VolumeInspect { name } => {
                validate_volume_name(name)?;
                self.permit_volume(name, capability)?;
                Ok(Reply::Volume(port.inspect(name)?))
            }
            Request::VolumeCreate { name } => {
                validate_volume_name(name)?;
                if !self.volumes.create {
                    return Err(Failure::Denied {
                        capability: capability.as_str().into(),
                        detail: "volume creation is outside the consented resource scope".into(),
                    });
                }
                self.permit_volume(name, capability)?;
                Ok(Reply::Volume(port.create(name)?))
            }
            Request::VolumeRemove { name, generation } => {
                validate_volume_name(name)?;
                immutable_identity(generation, &[32], "volume generation")?;
                self.permit_volume(name, capability)?;
                port.remove(name, generation)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            _ => unreachable!(),
        }
    }

    fn permit_volume(&self, name: &str, capability: Capability) -> Result<(), Failure> {
        if self.volumes.permits(name) {
            Ok(())
        } else {
            Err(Failure::Denied {
                capability: capability.as_str().into(),
                detail: "volume is outside the consented resource scope".into(),
            })
        }
    }

    fn networks(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let capability = request.capability();
        let port = self.peer.authority().port(capability, services.networks)?;
        match request {
            Request::NetworkList => Ok(Reply::Networks(self.visible_networks(port.list()?))),
            Request::NetworkInspect { reference } => {
                self.permit_network_reference(reference, capability)?;
                let network = port.inspect(reference)?;
                self.permit_network(&network, capability)?;
                Ok(Reply::Network(network))
            }
            Request::NetworkCreate { name } => {
                if !self.networks.create {
                    return Err(Failure::Denied {
                        capability: capability.as_str().into(),
                        detail: "network creation is outside the consented resource scope".into(),
                    });
                }
                Ok(Reply::Identity(port.create(name)?))
            }
            Request::NetworkRemove { reference } => {
                immutable_identity(reference, &[32], "network")?;
                self.permit_network_reference(reference, capability)?;
                let network = port.inspect(reference)?;
                self.permit_network(&network, capability)?;
                port.remove(reference).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::NetworkConnect {
                reference,
                container,
                aliases,
            } => {
                validate_endpoint_aliases(aliases)?;
                let reference = immutable_reference(reference, &[32], "network")?;
                immutable_identity(container, &[32, 64], "container")?;
                self.permit_network_reference(reference, capability)?;
                let network = port.inspect(reference)?;
                self.permit_network(&network, capability)?;
                let target = self.resolve_container_for(container, services.containers, Capability::NetworkWrite)?;
                port.connect_with_aliases(reference, &target.id, aliases)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::NetworkDisconnect { reference, container } => {
                let reference = immutable_reference(reference, &[32], "network")?;
                immutable_identity(container, &[32, 64], "container")?;
                self.permit_network_reference(reference, capability)?;
                let network = port.inspect(reference)?;
                self.permit_network(&network, capability)?;
                let target = self.resolve_container_for(container, services.containers, Capability::NetworkWrite)?;
                port.disconnect(reference, &target.id)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            _ => unreachable!(),
        }
    }

    fn permit_network(&self, network: &crate::port::NetworkSummary, capability: Capability) -> Result<(), Failure> {
        self.networks
            .permits(&network.id, &network.name)
            .then_some(())
            .ok_or_else(|| Failure::Denied {
                capability: capability.as_str().into(),
                detail: "network is outside the consented resource scope".into(),
            })
    }

    fn permit_network_reference(&self, reference: &str, capability: Capability) -> Result<(), Failure> {
        if !resource_name(reference) && immutable_identity(reference, &[32], "network").is_err() {
            return Err(Failure::Conflict {
                detail: "network reference must be an exact name or complete immutable ID".into(),
            });
        }
        if self.networks.permits_reference(reference) {
            Ok(())
        } else {
            Err(Failure::Denied {
                capability: capability.as_str().into(),
                detail: "network is outside the consented resource scope".into(),
            })
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
            Request::WorkspaceInspect { name } => Ok(Reply::WorkspaceConfiguration(
                self.visible_workspace_for(name, port.inspect(name)?),
            )),
            Request::WorkspaceCreate { configuration } => Ok(Reply::WorkspaceConfiguration(
                self.visible_workspace(port.create(configuration)?),
            )),
            Request::WorkspaceUpdate {
                name,
                generation,
                configuration_revision,
                configuration,
            } => {
                immutable_identity(generation, &[32], "workspace generation")?;
                immutable_identity(configuration_revision, &[32], "workspace configuration revision")?;
                Ok(Reply::WorkspaceConfiguration(self.visible_workspace(port.update(
                    name,
                    generation,
                    configuration_revision,
                    configuration,
                )?)))
            }
            Request::WorkspaceEnvironmentPatch {
                name,
                generation,
                configuration_revision,
                patch,
            } => {
                immutable_identity(generation, &[32], "workspace generation")?;
                immutable_identity(configuration_revision, &[32], "workspace configuration revision")?;
                workspace_environment_patch(patch)?;
                if patch
                    .set
                    .iter()
                    .map(|(variable, _)| variable)
                    .chain(patch.remove.iter())
                    .any(|variable| !self.workspace_environment.permits_write(name, variable))
                {
                    return Err(Failure::Denied {
                        capability: Capability::WorkspaceEnvironmentWrite.as_str().into(),
                        detail: "workspace environment name is outside the consented write scope".into(),
                    });
                }
                Ok(Reply::WorkspaceEnvironmentPatch(port.patch_environment(
                    name,
                    generation,
                    configuration_revision,
                    patch,
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

    fn visible_workspace(&self, configuration: WorkspaceConfiguration) -> WorkspaceConfiguration {
        let name = configuration.name.clone();
        self.visible_workspace_for(&name, configuration)
    }

    fn visible_workspace_for(
        &self,
        workspace: &str,
        mut configuration: WorkspaceConfiguration,
    ) -> WorkspaceConfiguration {
        let permitted = self
            .peer
            .authority()
            .granted::<Capability>()
            .holds(Capability::WorkspaceEnvironmentRead);
        let original = configuration.environment.len();
        configuration
            .environment
            .retain(|(variable, _)| permitted && self.workspace_environment.permits_read(workspace, variable));
        configuration.environment_redacted = configuration.environment.len() != original;
        configuration
    }

    fn extensions(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        let port = self.peer.authority().port(request.capability(), services.extensions)?;
        match request {
            Request::ExtensionList => Ok(Reply::Extensions(port.list()?)),
            Request::ExtensionCatalogue => {
                let catalogue = port.catalogue()?;
                catalogue.validate()?;
                Ok(Reply::ExtensionCatalogue(catalogue))
            }
            Request::ExtensionInspect { name } => Ok(Reply::Extension(port.inspect(name)?)),
            Request::ExtensionEnable { name, image_digest } => {
                immutable_digest(image_digest, "extension image")?;
                port.enable(name, image_digest)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExtensionDisable { name, image_digest } => {
                immutable_digest(image_digest, "extension image")?;
                port.disable(name, image_digest)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExtensionRetry { name, image_digest } => {
                immutable_digest(image_digest, "extension image")?;
                port.retry(name, image_digest)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExtensionRemove { name, image_digest } => {
                immutable_digest(image_digest, "extension image")?;
                port.remove(name, image_digest)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExtensionAcquisitionStart { reference } => {
                acquisition_reference(reference)?;
                Ok(Reply::ExtensionAcquisitionJob(port.acquisition_start(reference)?))
            }
            Request::ExtensionAcquisitionStatus { job } => {
                acquisition_job(job)?;
                Ok(Reply::ExtensionAcquisition(port.acquisition_status(job)?))
            }
            Request::ExtensionAcquisitionCancel { job, revision } => {
                acquisition_job(job)?;
                port.acquisition_cancel(job, *revision)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::ExtensionInstall {
                job,
                revision,
                image_digest,
                granted,
                containers,
                images,
                networks,
                volumes,
                filesystem,
                workspace_environment,
            } => {
                acquisition_job(job)?;
                immutable_digest(image_digest, "extension candidate image")?;
                Ok(Reply::Extension(port.install(
                    job,
                    *revision,
                    image_digest,
                    granted,
                    containers,
                    images,
                    networks,
                    volumes,
                    filesystem,
                    workspace_environment,
                )?))
            }
            Request::ExtensionUpdate {
                job,
                revision,
                image_digest,
                granted,
                containers,
                images,
                networks,
                volumes,
                filesystem,
                workspace_environment,
            } => {
                acquisition_job(job)?;
                immutable_digest(image_digest, "extension candidate image")?;
                Ok(Reply::Extension(port.update(
                    job,
                    *revision,
                    image_digest,
                    granted,
                    containers,
                    images,
                    networks,
                    volumes,
                    filesystem,
                    workspace_environment,
                )?))
            }
            _ => Err(Failure::Unsupported {
                call: "extension management".into(),
            }),
        }
    }

    fn terminal(&mut self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if matches!(
            request,
            Request::TerminalCommandStart { .. }
                | Request::TerminalCommandInspect { .. }
                | Request::TerminalCommandOutput { .. }
                | Request::TerminalCommandWait { .. }
                | Request::TerminalCommandCancel { .. }
                | Request::TerminalCommandWrite { .. }
                | Request::TerminalCommandCloseInput { .. }
        ) {
            return self.terminal_command(request, services);
        }
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
        // Compound operations must pass every check before the host port is
        // obtained: opening/closing changes layout and process lifetime, while
        // occupant switching replaces what runs in an existing slot.
        if matches!(
            request,
            Request::TerminalOpenTab { .. }
                | Request::TerminalSplit { .. }
                | Request::TerminalSplitObserved { .. }
                | Request::TerminalClosePane { .. }
                | Request::TerminalClosePaneObserved { .. }
                | Request::TerminalSwitchOccupant { .. }
                | Request::TerminalSwitchOccupantObserved { .. }
        ) {
            self.peer.authority().permit(Capability::TerminalLayoutControl)?;
            self.peer.authority().permit(Capability::TerminalProcessControl)?;
        }
        let port = self.peer.authority().port(request.capability(), services.terminal)?;
        Self::command(request, port.port())
    }

    fn terminal_command(&mut self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        if let Request::TerminalCommandStart {
            slot,
            generation,
            revision,
            command,
            working_directory,
            stdin,
        } = request
        {
            validate_terminal_command(command)?;
            if working_directory.as_ref().is_some_and(|directory| {
                directory.is_empty()
                    || !directory.starts_with('/')
                    || directory.contains('\0')
                    || directory.len() > 4096
            }) {
                return Err(Failure::Conflict {
                    detail:
                        "terminal command working_directory must be an absolute, NUL-free path of at most 4096 bytes"
                            .into(),
                });
            }
            if *stdin {
                self.peer.authority().permit(Capability::TerminalInput)?;
            }
            let terminal = self
                .peer
                .authority()
                .port(Capability::TerminalProcessControl, services.terminal)?;
            let observed = terminal.read(slot, 0)?;
            if observed.generation != *generation || observed.revision != *revision {
                return Err(Failure::Conflict {
                    detail: format!(
                        "stale pane identity for {slot}: expected {generation}/{revision}, current {}/{}",
                        observed.generation, observed.revision
                    ),
                });
            }
            let container = services.containers.inspect("workspace")?;
            let id = services.control.execute(
                "workspace",
                &container.id,
                container.generation,
                command,
                &[],
                None,
                working_directory.as_deref(),
                *stdin,
            )?;
            immutable_identity(&id, &[32], "terminal command")?;
            let execution = services.containers.execution(&id)?;
            if execution.container_id != container.id {
                return Err(Failure::Failed {
                    detail: "host returned a terminal command owned by another container".into(),
                });
            }
            return Ok(Reply::TerminalCommand(terminal_command_summary(
                execution,
                slot,
                *generation,
                *revision,
            )));
        }

        let (id, slot, generation, revision) = match request {
            Request::TerminalCommandInspect {
                id,
                slot,
                generation,
                revision,
            }
            | Request::TerminalCommandOutput {
                id,
                slot,
                generation,
                revision,
                ..
            }
            | Request::TerminalCommandWait {
                id,
                slot,
                generation,
                revision,
                ..
            }
            | Request::TerminalCommandCancel {
                id,
                slot,
                generation,
                revision,
                ..
            }
            | Request::TerminalCommandWrite {
                id,
                slot,
                generation,
                revision,
                ..
            }
            | Request::TerminalCommandCloseInput {
                id,
                slot,
                generation,
                revision,
            } => (id, slot, *generation, *revision),
            _ => unreachable!(),
        };
        immutable_identity(id, &[32], "terminal command")?;
        // The pane snapshot fences creation. Once started, the returned command ID is the durable
        // authority: replacing or closing its originating pane must not make output, completion,
        // input shutdown, or cancellation unreachable.
        match request {
            Request::TerminalCommandInspect { .. } => {
                let execution = services.containers.execution(id)?;
                Ok(Reply::TerminalCommand(terminal_command_summary(
                    execution, slot, generation, revision,
                )))
            }
            Request::TerminalCommandOutput { after, limit, .. } => {
                if *limit == 0 || *limit > 16 {
                    return Err(Failure::Conflict {
                        detail: "terminal command output limit must be between 1 and 16".into(),
                    });
                }
                let page = services.containers.execution_output(id, *after, *limit)?;
                validate_execution_output(&page, *after, *limit)?;
                Ok(Reply::TerminalCommandOutput(crate::port::TerminalCommandOutput {
                    id: id.clone(),
                    slot: slot.clone(),
                    generation,
                    revision,
                    output: page,
                }))
            }
            Request::TerminalCommandWait { timeout_ms, .. } => {
                if !(1..=30_000).contains(timeout_ms) {
                    return Err(Failure::Conflict {
                        detail: "terminal command wait timeout_ms must be between 1 and 30000".into(),
                    });
                }
                let execution = services.containers.execution_wait(id, *timeout_ms)?;
                Ok(Reply::TerminalCommand(terminal_command_summary(
                    execution, slot, generation, revision,
                )))
            }
            Request::TerminalCommandCancel { signal, timeout_ms, .. } => {
                bounded_signal(signal)?;
                if !(1..=30_000).contains(timeout_ms) {
                    return Err(Failure::Conflict {
                        detail: "terminal command cancellation timeout_ms must be between 1 and 30000".into(),
                    });
                }
                services.control.execution_cancel(id, signal, *timeout_ms)?;
                let execution = services.containers.execution(id)?;
                Ok(Reply::TerminalCommand(terminal_command_summary(
                    execution, slot, generation, revision,
                )))
            }
            Request::TerminalCommandWrite { contents, .. } => {
                if contents.is_empty() || contents.len() > PANE_INPUT_BYTES {
                    return Err(Failure::Conflict {
                        detail: format!("terminal command input must contain between 1 and {PANE_INPUT_BYTES} bytes"),
                    });
                }
                let execution = services.containers.execution(id)?;
                if !execution.running {
                    return Err(Failure::Conflict {
                        detail: "terminal command input is only writable while it is running".into(),
                    });
                }
                services.control.execution_write(id, contents)?;
                Ok(Reply::TerminalCommandInput(crate::port::TerminalCommandInput {
                    id: id.clone(),
                    committed: u32::try_from(contents.len()).expect("pane input bound fits u32"),
                }))
            }
            Request::TerminalCommandCloseInput { .. } => {
                let execution = services.containers.execution(id)?;
                if !execution.running {
                    return Err(Failure::Conflict {
                        detail: "terminal command input can only be closed while it is running".into(),
                    });
                }
                services.control.execution_close_input(id)?;
                Ok(Reply::Done)
            }
            Request::TerminalCommandStart { .. } => unreachable!(),
            _ => unreachable!(),
        }
    }

    fn command(request: &Request, port: &dyn TerminalSurface) -> Result<Reply, Failure> {
        match request {
            Request::TerminalOpenTab { title } => Ok(Reply::Identity(port.open_tab(title)?)),
            Request::TerminalPinTab { tab, pinned } => {
                port.pin_tab(tab, *pinned).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalSplit { slot, division } => Ok(Reply::Identity(port.split(slot, *division)?)),
            Request::TerminalSplitObserved { slot, division, .. } => Ok(Reply::Identity(port.split(slot, *division)?)),
            Request::TerminalSpawn { slot, command } => {
                validate_terminal_command(command)?;
                port.spawn(slot, command).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalSpawnObserved { slot, command, .. } => {
                validate_terminal_command(command)?;
                port.spawn(slot, command).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalWritePane {
                slot,
                generation,
                revision,
                contents,
            } => {
                if contents.len() > PANE_INPUT_BYTES {
                    return Err(Failure::Conflict {
                        detail: format!("terminal input exceeds the {PANE_INPUT_BYTES} byte limit"),
                    });
                }
                port.write(slot, *generation, *revision, contents)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            Request::TerminalResizeGrid { slot, columns, rows }
            | Request::TerminalResizeGridObserved {
                slot, columns, rows, ..
            } => {
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
            Request::TerminalClosePaneObserved { slot, .. } => {
                port.close(slot).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalFocusPane { slot } | Request::TerminalFocusPaneObserved { slot, .. } => {
                port.focus(slot).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalRetitlePane { slot, title } | Request::TerminalRetitlePaneObserved { slot, title, .. } => {
                validate_pane_title(title)?;
                port.retitle(slot, title).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalRatio { slot, ratio } => {
                port.ratio(slot, *ratio).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalRatioObserved { slot, ratio, .. } => {
                port.ratio(slot, *ratio).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::TerminalSwitchOccupant {
                slot,
                generation,
                target,
            }
            | Request::TerminalSwitchOccupantObserved {
                slot,
                generation,
                target,
                ..
            } => {
                if let crate::port::PaneOccupantTarget::Surface { extension, provider } = target {
                    crate::ExtensionName::new(extension.clone()).map_err(|_| Failure::Conflict {
                        detail: "invalid extension name".into(),
                    })?;
                    crate::ExtensionName::new(provider.clone()).map_err(|_| Failure::Conflict {
                        detail: "invalid pane provider name".into(),
                    })?;
                }
                port.switch_occupant(slot, *generation, target)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
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
            Request::FilesystemInventory => {
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::FileInventory(port.inventory(&self.filesystem.read)?))
            }
            Request::FilesystemChanges { observed, after, limit } => {
                if observed.len() != 32 || !observed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(Failure::Failed {
                        detail: "filesystem journal identity must be 32 hexadecimal characters".into(),
                    });
                }
                if *limit == 0 || *limit > 256 {
                    return Err(Failure::Failed {
                        detail: "filesystem change page limit must be from 1 through 256".into(),
                    });
                }
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::FileChanges(port.changes_since(
                    &self.filesystem.read,
                    observed,
                    *after,
                    usize::from(*limit),
                )?))
            }
            Request::FilesystemList { path } => {
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Entries(port.list(path)?))
            }
            Request::FilesystemListPage {
                path,
                after,
                observed,
                limit,
            } => {
                if *limit == 0 || *limit > 256 || observed.as_ref().is_some_and(|value| value.len() > 256) {
                    return Err(Failure::Failed {
                        detail: "filesystem directory page limit or observed identity exceeds bounds".into(),
                    });
                }
                if after.is_some() != observed.is_some() {
                    return Err(Failure::Failed {
                        detail: "filesystem directory continuation requires both cursor and observed identity".into(),
                    });
                }
                if after.as_ref().is_some_and(|cursor| {
                    let directory = path.as_str();
                    let value = cursor.as_str();
                    let child = if directory.is_empty() {
                        Some(value)
                    } else {
                        value.strip_prefix(directory).and_then(|rest| rest.strip_prefix('/'))
                    };
                    child.is_none_or(|name| name.is_empty() || name.contains('/'))
                }) {
                    return Err(Failure::Failed {
                        detail: "filesystem directory cursor must name a child after the directory".into(),
                    });
                }
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::DirectoryPage(port.list_page(
                    path,
                    after.as_ref(),
                    observed.as_deref(),
                    *limit,
                )?))
            }
            Request::FilesystemRead { path } => {
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Contents(port.read(path)?))
            }
            Request::FilesystemReadRange {
                path,
                offset,
                limit,
                observed,
            } => {
                if *limit == 0 || *limit > 64 * 1024 || observed.as_ref().is_some_and(|value| value.len() > 256) {
                    return Err(Failure::Failed {
                        detail: "filesystem range exceeds protocol bounds".into(),
                    });
                }
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::FileRange(port.read_range(
                    path,
                    *offset,
                    *limit,
                    observed.as_deref(),
                )?))
            }
            Request::FilesystemReadRanges { ranges } => {
                if ranges.is_empty()
                    || ranges.len() > 64
                    || ranges.iter().any(|range| {
                        range.limit == 0
                            || range.limit > 64 * 1024
                            || range.observed.as_ref().is_some_and(|value| value.len() > 256)
                    })
                    || ranges.iter().map(|range| range.limit).sum::<usize>() > 64 * 1024
                {
                    return Err(Failure::Failed {
                        detail: "filesystem range batch exceeds protocol bounds".into(),
                    });
                }
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                let values = ranges
                    .iter()
                    .map(|range| port.read_range(&range.path, range.offset, range.limit, range.observed.as_deref()))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Reply::FileRanges(values))
            }
            Request::FilesystemStat { path } => {
                let port = self.peer.authority().port(Capability::FilesystemRead, services.files)?;
                Ok(Reply::Entry(port.stat(path)?))
            }
            Request::FilesystemWrite { path, contents } => {
                if contents.len() > 64 * 1024 {
                    return Err(Failure::Failed {
                        detail: "filesystem write exceeds 65536 bytes".into(),
                    });
                }
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.write(path, contents).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::FilesystemWriteObserved {
                path,
                observed,
                contents,
            } => {
                if contents.len() > 64 * 1024 || observed.is_empty() || observed.len() > 256 {
                    return Err(Failure::Failed {
                        detail: "observed filesystem write exceeds protocol bounds".into(),
                    });
                }
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                Ok(Reply::Identity(port.write_observed(path, observed, contents)?))
            }
            Request::FilesystemCreateObserved { path, contents } => {
                if contents.len() > 64 * 1024 {
                    return Err(Failure::Failed {
                        detail: "filesystem creation exceeds 65536 bytes".into(),
                    });
                }
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                Ok(Reply::Identity(port.create_observed(path, contents)?))
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
            Request::FilesystemRenameObserved { from, to, observed } => {
                if observed.is_empty() || observed.len() > 256 {
                    return Err(Failure::Failed {
                        detail: "filesystem observation exceeds protocol bounds".into(),
                    });
                }
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                Ok(Reply::Identity(port.rename_observed(from, to, observed)?))
            }
            Request::FilesystemRemove { path } => {
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.remove(path).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::FilesystemRemoveObserved { path, observed } => {
                if observed.is_empty() || observed.len() > 256 {
                    return Err(Failure::Failed {
                        detail: "filesystem observation exceeds protocol bounds".into(),
                    });
                }
                let port = self
                    .peer
                    .authority()
                    .port(Capability::FilesystemWrite, services.files)?;
                port.remove_observed(path, observed)
                    .map(|()| Reply::Done)
                    .map_err(Failure::from)
            }
            _ => Err(Failure::Unsupported {
                call: "filesystem".into(),
            }),
        }
    }

    fn state(&self, request: &Request, services: &Services<'_>) -> Result<Reply, Failure> {
        const LIMIT: usize = 1024 * 1024;
        let capability = request.capability();
        let port = self.peer.authority().port(capability, services.state)?;
        match request {
            Request::StateRead => {
                let state = port.read()?;
                exact_state_identity(&state.identity).map_err(|_| Failure::Failed {
                    detail: "host returned an invalid extension state identity".into(),
                })?;
                if state.contents.len() > LIMIT {
                    return Err(Failure::Failed {
                        detail: "extension state exceeds the 1 MiB quota".into(),
                    });
                }
                Ok(Reply::State(state))
            }
            Request::StateWrite { observed, contents } => {
                if contents.len() > LIMIT {
                    return Err(Failure::Conflict {
                        detail: "extension state exceeds the 1 MiB quota".into(),
                    });
                }
                exact_state_identity(observed)?;
                port.write(observed, contents)
                    .map(Reply::Identity)
                    .map_err(Failure::from)
            }
            Request::StateClear { observed } => {
                exact_state_identity(observed)?;
                port.clear(observed).map(|()| Reply::Done).map_err(Failure::from)
            }
            Request::PreferenceRead => {
                let preferences = port.preferences()?;
                validate_preferences(&preferences)?;
                Ok(Reply::Preferences(preferences))
            }
            Request::PreferenceSet { observed, key, value } => {
                validate_preference(key, value)?;
                port.preference_set(*observed, key, value)
                    .map(Reply::Revision)
                    .map_err(Failure::from)
            }
            Request::PreferenceRemove { observed, key } => {
                validate_preference_key(key)?;
                port.preference_remove(*observed, key)
                    .map(Reply::Revision)
                    .map_err(Failure::from)
            }
            Request::CredentialRead { key } => {
                validate_credential_key(key)?;
                let credential = port.credential(key)?;
                if credential.key != *key {
                    return Err(Failure::Failed {
                        detail: "host returned a credential for another key".into(),
                    });
                }
                if credential
                    .value
                    .as_ref()
                    .is_some_and(|value| value.len() > CREDENTIAL_VALUE_BYTES)
                {
                    return Err(Failure::Failed {
                        detail: "host returned an oversized credential".into(),
                    });
                }
                Ok(Reply::Credential(credential))
            }
            Request::CredentialSet { observed, key, value } => {
                validate_credential_key(key)?;
                if value.len() > CREDENTIAL_VALUE_BYTES {
                    return Err(Failure::Conflict {
                        detail: "credentials are limited to 64 KiB".into(),
                    });
                }
                port.credential_set(*observed, key, value)
                    .map(Reply::Revision)
                    .map_err(Failure::from)
            }
            Request::CredentialRemove { observed, key } => {
                validate_credential_key(key)?;
                port.credential_remove(*observed, key)
                    .map(Reply::Revision)
                    .map_err(Failure::from)
            }
            _ => Err(Failure::Unsupported {
                call: "extension state".into(),
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

fn validate_notification(notification: &crate::port::Notification) -> Result<(), Failure> {
    let valid =
        |value: &str, max: usize| !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control);
    if !valid(&notification.id, 128) || !valid(&notification.title, 256) || !valid(&notification.body, 4096) {
        return Err(Failure::Conflict {
            detail: "notification id, title, or body is empty, oversized, or contains control characters".into(),
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum FilesystemAccess {
    List,
    Read,
    Write,
    Create,
    Delete,
    Rename,
}

fn filesystem_access(request: &Request) -> FilesystemAccess {
    match request {
        Request::FilesystemList { .. } | Request::FilesystemListPage { .. } => FilesystemAccess::List,
        Request::FilesystemWrite { .. } | Request::FilesystemWriteObserved { .. } => FilesystemAccess::Write,
        Request::FilesystemCreateObserved { .. } | Request::FilesystemMkdir { .. } => FilesystemAccess::Create,
        Request::FilesystemRemove { .. } | Request::FilesystemRemoveObserved { .. } => FilesystemAccess::Delete,
        Request::FilesystemRename { .. } | Request::FilesystemRenameObserved { .. } => FilesystemAccess::Rename,
        _ => FilesystemAccess::Read,
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

fn validate_endpoint_aliases(aliases: &[String]) -> Result<(), Failure> {
    let valid = aliases.len() <= 64
        && aliases.iter().all(|value| {
            !value.is_empty()
                && value.len() <= 253
                && value
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| byte.is_ascii_alphanumeric() || (index != 0 && b"_.-".contains(&byte)))
        })
        && aliases.iter().collect::<std::collections::BTreeSet<_>>().len() == aliases.len();
    if valid {
        Ok(())
    } else {
        Err(Failure::Conflict {
            detail: "network endpoint aliases must be at most 64 unique, 1..=253-byte ASCII endpoint names".into(),
        })
    }
}

fn validate_exec_environment(environment: &[(String, crate::ExecEnvironmentValue)]) -> Result<(), Failure> {
    let names: std::collections::BTreeSet<&str> = environment.iter().map(|(name, _)| name.as_str()).collect();
    let valid = environment.len() <= 256
        && names.len() == environment.len()
        && environment.iter().all(|(name, value)| {
            !name.is_empty()
                && name.len() <= 256
                && !name.contains(['=', '\0'])
                && value.as_str().len() <= 8192
                && !value.as_str().contains('\0')
        });
    let aggregate = environment
        .iter()
        .map(|(name, value)| name.len() + value.as_str().len())
        .sum::<usize>();
    if valid && aggregate <= 65_536 {
        Ok(())
    } else {
        Err(Failure::Conflict {
            detail: "exec environment must contain at most 256 unique NUL-free pairs and 65536 UTF-8 bytes; names are nonempty, exclude '=', and are at most 256 bytes; values are at most 8192 bytes".into(),
        })
    }
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
        detail: format!("{noun} operation requires the complete immutable ID returned by inspection"),
    })
}

fn validate_volume_name(name: &str) -> Result<(), Failure> {
    if resource_name(name) {
        Ok(())
    } else {
        Err(Failure::Conflict {
            detail: "volume name must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 255 bytes".into(),
        })
    }
}

fn resource_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_alphanumeric() || (index != 0 && b"_.-".contains(&byte)))
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

fn exact_state_identity(value: &str) -> Result<(), Failure> {
    if value == "absent" {
        return Ok(());
    }
    immutable_digest(value, "extension state").map_err(|_| Failure::Conflict {
        detail: "extension state mutation requires the exact identity returned by state.read()".into(),
    })
}

fn validate_pane_title(title: &str) -> Result<(), Failure> {
    if !title.trim().is_empty() && title.len() <= 256 && !title.chars().any(char::is_control) {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: "pane title must be nonblank and contain at most 256 UTF-8 bytes without control characters".into(),
    })
}

fn validate_semantic_tree(requested_slot: &str, tree: &crate::port::PaneSemanticTree) -> Result<(), Failure> {
    use crate::port::{SEMANTIC_DEPTH_LIMIT, SEMANTIC_NODE_LIMIT, SEMANTIC_TEXT_LIMIT};

    if tree.slot != requested_slot {
        return Err(Failure::Conflict {
            detail: "pane semantic reply does not match the requested slot".into(),
        });
    }
    let mut nodes = Vec::from([(&tree.root, 0usize)]);
    let mut identities = std::collections::BTreeSet::new();
    let mut count = 0usize;
    while let Some((node, depth)) = nodes.pop() {
        count += 1;
        if count > SEMANTIC_NODE_LIMIT || depth > SEMANTIC_DEPTH_LIMIT {
            return Err(Failure::Conflict {
                detail: "pane semantic reply exceeds its bounded shape".into(),
            });
        }
        if !identities.insert(node.id) {
            return Err(Failure::Conflict {
                detail: "pane semantic reply contains duplicate node identities".into(),
            });
        }
        if node.role.is_empty()
            || node.role.len() > SEMANTIC_TEXT_LIMIT
            || node.label.as_ref().is_some_and(|text| text.len() > SEMANTIC_TEXT_LIMIT)
            || node.value.as_ref().is_some_and(|text| text.len() > SEMANTIC_TEXT_LIMIT)
            || node.actions.len() > 6
        {
            return Err(Failure::Conflict {
                detail: "pane semantic reply contains an invalid node".into(),
            });
        }
        if node
            .actions
            .iter()
            .enumerate()
            .any(|(at, action)| node.actions[..at].contains(action))
        {
            return Err(Failure::Conflict {
                detail: "pane semantic reply contains duplicate node actions".into(),
            });
        }
        nodes.extend(node.children.iter().map(|child| (child, depth + 1)));
    }
    Ok(())
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

    let container_name = |value: &str| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte.is_ascii_alphanumeric() || (index != 0 && b"_.-".contains(&byte)))
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
    let environment_name = |value: &str| !value.is_empty() && value.len() <= 256 && !value.contains(['=', '\0']);
    let valid = container_name(&spec.name)
        && spec.hostname.as_ref().is_none_or(|value| {
            !value.is_empty()
                && value.len() <= 253
                && value
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| byte.is_ascii_alphanumeric() || (index != 0 && b"_.-".contains(&byte)))
        })
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
            .all(|mount| resource_name(&mount.volume) && absolute(&mount.target))
        && spec.network.as_ref().is_none_or(|network| resource_name(network))
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

fn validate_container_name(name: &str) -> Result<(), Failure> {
    let mut bytes = name.bytes();
    if bytes.next().is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name.len() <= 128
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Ok(());
    }
    Err(Failure::Conflict {
        detail: "container name must contain 1..=128 ASCII alphanumeric, underscore, period, or hyphen bytes".into(),
    })
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

#[cfg(test)]
mod immutable_identity_tests {
    use super::immutable_identity;

    #[test]
    fn container_execution_refuses_names_prefixes_and_malformed_ids() {
        for value in ["worker", "abcdef123456", &"A".repeat(32), &"g".repeat(64)] {
            assert!(
                immutable_identity(value, &[32, 64], "container").is_err(),
                "accepted {value}"
            );
        }
        assert!(immutable_identity(&"a".repeat(32), &[32, 64], "container").is_ok());
        assert!(immutable_identity(&"b".repeat(64), &[32, 64], "container").is_ok());
    }
}
