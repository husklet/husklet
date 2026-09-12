//! The real supply: one workspace, its records, and its container daemon.
//!
//! Everything in this file needs a running workspace, which is why it is the
//! one part of the host the suite cannot drive. It is kept apart from the
//! orchestration for exactly that reason: what can be tested and what cannot
//! are not mixed in one file.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use hl_extension::port::{
    Division, HostError, PaneText, TabSummary, TerminalSurface, WorkspaceConfiguration, WorkspaceControl,
    WorkspaceInventory, WorkspaceMount, WorkspaceState, WorkspaceTerminal,
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
        events: super::Events,
        audience: super::Audience,
    ) -> Self {
        Self::open(
            Workspace::extension(workspace, name)
                .through(terminal)
                .observing(events),
            audience,
        )
    }

    /// Runs a checked-out extension as a host process for deterministic GUI
    /// diagnostics. The socket, handshake, authority, services, and renderer
    /// are production paths; only OCI acquisition and container supervision
    /// are bypassed.
    #[doc(hidden)]
    #[cfg(debug_assertions)]
    #[must_use]
    pub fn local_extension(
        workspace: &WorkspaceConfig,
        name: &ExtensionName,
        terminal: Arc<dyn TerminalSurface + Send + Sync>,
        events: super::Events,
        entrypoint: impl Into<PathBuf>,
        initial_section: Option<&str>,
        audience: super::Audience,
    ) -> Self {
        Self::open(
            LocalWorkspace::new(workspace, name, entrypoint.into(), initial_section)
                .through(terminal)
                .observing(events),
            audience,
        )
    }
}

/// Exact runtime ownership captured before an uninstall forgets its record.
pub struct ExtensionRemoval {
    bridge: Option<Arc<Bridge>>,
    spec: SidecarSpec,
}

impl ExtensionRemoval {
    /// Retires the captured runtime while its durable record still exists.
    pub fn retire(&self) -> Result<(), HostError> {
        if let Some(bridge) = &self.bridge {
            Sidecar::new(Arc::clone(bridge)).remove_owned(&self.spec)?;
        }
        Ok(())
    }

    /// Deletes private data only after the durable record was forgotten.
    pub fn purge(self) -> Result<(), HostError> {
        self.spec
            .purge_data()
            .map_err(|error| HostError::Failed(error.to_string()))
    }
}

#[cfg(debug_assertions)]
struct LocalWorkspace {
    workspace: Workspace,
    entrypoint: PathBuf,
    initial_section: Option<String>,
    child: Mutex<Option<std::process::Child>>,
}

#[cfg(debug_assertions)]
impl LocalWorkspace {
    fn new(
        workspace: &WorkspaceConfig,
        name: &ExtensionName,
        entrypoint: PathBuf,
        initial_section: Option<&str>,
    ) -> Self {
        Self {
            workspace: Workspace::extension(workspace, name),
            entrypoint,
            initial_section: initial_section.map(str::to_owned),
            child: Mutex::new(None),
        }
    }

    fn through(mut self, terminal: Arc<dyn TerminalSurface + Send + Sync>) -> Self {
        self.workspace = self.workspace.through(terminal);
        self
    }

    fn observing(mut self, events: super::Events) -> Self {
        self.workspace = self.workspace.observing(events);
        self
    }
}

#[cfg(debug_assertions)]
impl Supply for LocalWorkspace {
    fn plan(&self) -> Result<Option<Plan>, String> {
        let Some(record) = self.workspace.record()? else {
            return Ok(None);
        };
        let manifest = described(&record);
        let image = Image {
            reference: self.entrypoint.display().to_string(),
            digest: record.image_digest.clone(),
            entrypoint: vec![self.entrypoint.display().to_string()],
            command: Vec::new(),
            user: String::new(),
        };
        let spec = SidecarSpec::new(&manifest, &record.granted, &image, self.workspace.socket(&record.name))
            .generation(record.installed_at);
        Ok(Some(Plan {
            record,
            manifest,
            spec,
            workspace: self.workspace.config.name.clone(),
        }))
    }

    fn ensure(&self, plan: &Plan) -> Result<(), String> {
        let mut command = std::process::Command::new("node");
        command
            .arg(&self.entrypoint)
            .env("HUSKLET_EXTENSION_SOCKET", plan.spec.socket());
        if let Some(section) = &self.initial_section {
            command.env("HUSKLET_TOP_SECTION", section);
        }
        let child = command
            .spawn()
            .map_err(|error| format!("start local extension {}: {error}", self.entrypoint.display()))?;
        *self.child.lock().unwrap_or_else(PoisonError::into_inner) = Some(child);
        Ok(())
    }

    fn startup_failure(&self, _plan: &Plan) -> Result<Option<String>, String> {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        match child.as_mut().and_then(|child| child.try_wait().ok()).flatten() {
            Some(status) => Ok(Some(format!("local extension stopped with {status}"))),
            None => Ok(None),
        }
    }

    fn attend(&self, plan: &Plan, conversation: &mut Conversation) -> Result<(), String> {
        self.workspace.attend(plan, conversation)
    }

    fn halt(&self, _plan: &Plan) {
        if let Some(mut child) = self.child.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
    events: super::Events,
    /// One host speaks to one workspace daemon for its whole lifetime. Reusing
    /// this bridge keeps startup health observation to a cheap local inspect
    /// instead of re-entering domain setup on every bounded poll.
    bridge: Mutex<Option<Arc<Bridge>>>,
}

impl Workspace {
    /// Binds a supply to whichever extension of one workspace is enabled.
    #[must_use]
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            config: workspace.clone(),
            wanted: None,
            terminal: None,
            events: super::Events::default(),
            bridge: Mutex::new(None),
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
            events: super::Events::default(),
            bridge: Mutex::new(None),
        }
    }

    /// Points the supply's terminal calls at a surface the window owns.
    #[must_use]
    pub fn through(mut self, terminal: Arc<dyn TerminalSurface + Send + Sync>) -> Self {
        self.terminal = Some(terminal);
        self
    }

    #[must_use]
    pub fn observing(mut self, events: super::Events) -> Self {
        self.events = events;
        self
    }

    /// Where this workspace keeps its extension state and sockets.
    fn root(&self) -> PathBuf {
        self.config.storage_dir(&crate::paths::hl_root())
    }

    /// The socket one extension is given, in a directory of its own so
    /// [`SidecarSpec::prepare`] can confine it without touching anything else.
    fn socket(&self, name: &ExtensionName) -> PathBuf {
        self.root()
            .join("extensions")
            .join(name.as_str())
            .join("extension.sock")
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

    /// Deletes the sidecar owned by one still-installed record. The record is
    /// deliberately read before its caller forgets it: its digest, grant,
    /// limits, and socket are the authority for exact ownership.
    pub fn extension_removal(
        workspace: &WorkspaceConfig,
        name: &ExtensionName,
        image_digest: &str,
    ) -> Result<ExtensionRemoval, HostError> {
        let supply = Self::extension(workspace, name);
        let storage =
            hl_ws::storage::Directory::open(supply.root()).map_err(|error| HostError::Failed(error.to_string()))?;
        let records = Records::open(storage).map_err(|fault| HostError::Failed(fault.to_string()))?;
        let record = records
            .all()
            .map_err(|fault| HostError::Failed(fault.to_string()))?
            .into_iter()
            .find(|record| record.name == *name)
            .ok_or_else(|| HostError::Absent(format!("extension {name} is not installed")))?;
        if record.image_digest != image_digest {
            return Err(HostError::Conflict(format!(
                "extension {name} changed since it was inspected"
            )));
        }
        let manifest = described(&record);
        let bridge = supply.live_bridge().map_err(HostError::Failed)?;
        let image = match &bridge {
            Some(bridge) => supply.image(bridge, &record).map_err(HostError::Failed)?,
            None => Image {
                reference: record.image_digest.clone(),
                digest: record.image_digest.clone(),
                entrypoint: Vec::new(),
                command: Vec::new(),
                user: String::new(),
            },
        };
        let spec =
            SidecarSpec::new(&manifest, &record.granted, &image, supply.socket(name)).generation(record.installed_at);
        Ok(ExtensionRemoval { bridge, spec })
    }

    /// The workspace's own container daemon, started if it is not up.
    fn bridge(&self) -> Result<Arc<Bridge>, String> {
        let mut held = self.bridge.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(bridge) = held.as_ref() {
            return Ok(Arc::clone(bridge));
        }
        let domain = crate::runtime::domain::Domain::new(&self.config);
        let socket = domain.ensure(&self.config).map_err(|error| error.to_string())?;
        let bridge = Bridge::new(socket).map(Arc::new).map_err(|error| error.to_string())?;
        held.replace(Arc::clone(&bridge));
        Ok(bridge)
    }

    /// Connects to the workspace daemon only when it is already live.
    ///
    /// Host teardown follows workspace checkpoint shutdown, so it must never
    /// use the start-capable bridge and restore the domain it is closing.
    fn live_bridge(&self) -> Result<Option<Arc<Bridge>>, String> {
        let domain = crate::runtime::domain::Domain::new(&self.config);
        let Some(socket) = domain.live_socket().map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        Bridge::new(socket)
            .map(Arc::new)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// What the extension's image says about how to run it.
    fn image(&self, bridge: &Bridge, record: &Record) -> Result<Image, String> {
        let client = bridge.client();
        let inspection = bridge
            .wait(client.images().inspect(&record.image_digest))
            .map_err(|error| error.to_string())?;
        image_from_inspection(
            record.name.as_str(),
            &record.image_digest,
            self.config.arch.as_str(),
            &inspection,
        )
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
        let image = self.image(&bridge, &record)?;
        let spec = SidecarSpec::new(&manifest, &record.granted, &image, self.socket(&record.name))
            .generation(record.installed_at);
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

    fn startup_failure(&self, plan: &Plan) -> Result<Option<String>, String> {
        let bridge = self.bridge()?;
        let client = bridge.client();
        let container = bridge
            .wait(client.containers().inspect(plan.spec.container()))
            .map_err(|error| error.to_string())?;
        if container.state.activity.running {
            return Ok(None);
        }
        let state = &container.state;
        let detail = if !state.error.is_empty() {
            state.error.clone()
        } else if state.exit_code != 0 {
            format!("exit code {}", state.exit_code)
        } else {
            format!("status {}", state.status)
        };
        Ok(Some(format!(
            "{} stopped before rendering its first interface: {detail}",
            plan.record.name
        )))
    }

    /// # Errors
    /// Returns why the conversation ended early, including the failure to bind
    /// the ports it is served against.
    fn attend(&self, plan: &Plan, conversation: &mut Conversation) -> Result<(), String> {
        conversation.with_events(self.events.clone());
        let extensions = Extensions::open(&self.config).map_err(|error| error.to_string())?;
        let state = super::super::StateBlob::new(&self.root(), &plan.record.name).map_err(|error| error.to_string())?;
        conversation.with_extension_events(extensions.extension_events());
        let console = Console;
        let terminal: &dyn TerminalSurface = self.terminal.as_deref().unwrap_or(&console);
        let store = Store {
            current: self.config.name.clone(),
        };
        let notifications = conversation.notifications();
        let services = Services {
            workspace: self.describe(),
            workspaces: &store,
            workspace_control: &store,
            extensions: extensions.management(),
            containers: extensions.containers(),
            control: extensions.control(),
            images: extensions.images(),
            volumes: extensions.volumes(),
            networks: extensions.networks(),
            terminal,
            files: extensions.files(),
            state: &state,
            notifications: &notifications,
        };
        conversation.serve(&services).map_err(|fault| fault.to_string())
    }

    fn halt(&self, plan: &Plan) {
        let Ok(Some(bridge)) = self.live_bridge() else {
            return;
        };
        if let Err(error) = Sidecar::new(bridge).stop_owned(&plan.spec) {
            hl_log::hl_error!(hl_log::tag::RUNTIME, "extension {}: {error}", plan.record.name);
        }
    }
}

fn image_from_inspection(
    extension: &str,
    recorded_digest: &str,
    architecture: &str,
    inspection: &hl_client::model::InspectImage,
) -> Result<Image, String> {
    if inspection.id != recorded_digest {
        return Err(format!(
            "extension image inspection returned digest {}, expected recorded digest {recorded_digest}; retry workspace provisioning to restore the exact extension image",
            inspection.id
        ));
    }
    if inspection.os != "linux" || inspection.architecture != architecture {
        return Err(format!(
            "recorded extension image {recorded_digest} is {}/{}, but this workspace requires linux/{architecture}; retry workspace provisioning with the correct architecture",
            inspection.os, inspection.architecture
        ));
    }
    if extension == "top"
        && (inspection.config.entrypoint != ["/usr/local/bin/node"]
            || inspection.config.cmd != ["/app/dist/main.js"]
            || inspection.config.user != "node")
    {
        return Err(format!(
            "recorded Top image {recorded_digest} has an unexpected OCI process configuration; expected node to run /app/dist/main.js as the node user; verify the published image, then retry workspace provisioning"
        ));
    }
    Ok(Image::from_inspection(recorded_digest.to_owned(), inspection))
}

#[cfg(test)]
mod image_tests {
    use hl_client::model::{ImageConfig, InspectImage};

    use super::image_from_inspection;

    fn inspection(id: &str, os: &str, architecture: &str) -> InspectImage {
        InspectImage {
            id: id.to_owned(),
            repo_tags: Vec::new(),
            repo_digests: Vec::new(),
            created: String::new(),
            size: 0,
            virtual_size: 0,
            os: os.to_owned(),
            architecture: architecture.to_owned(),
            config: ImageConfig {
                entrypoint: vec!["/extension".to_owned()],
                ..ImageConfig::default()
            },
        }
    }

    #[test]
    fn startup_is_bound_to_the_recorded_digest_and_workspace_platform() {
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let image = image_from_inspection("sample", digest, "amd64", &inspection(digest, "linux", "amd64"))
            .expect("matching installed artifact");
        assert_eq!(image.reference, digest);
        assert_eq!(image.digest, digest);

        let wrong_digest = image_from_inspection(
            "sample",
            digest,
            "amd64",
            &inspection(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "linux",
                "amd64",
            ),
        )
        .expect_err("daemon returned a different image");
        assert!(wrong_digest.contains("expected recorded digest"));
        assert!(wrong_digest.contains("retry workspace provisioning"));

        for (os, architecture) in [("darwin", "amd64"), ("linux", "arm64")] {
            let wrong_platform =
                image_from_inspection("sample", digest, "amd64", &inspection(digest, os, architecture))
                    .expect_err("wrong platform");
            assert!(wrong_platform.contains(&format!("is {os}/{architecture}")));
            assert!(wrong_platform.contains("requires linux/amd64"));
            assert!(wrong_platform.contains("retry workspace provisioning"));
        }
    }

    #[test]
    fn top_startup_is_bound_to_the_dockerfile_process_contract() {
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut top = inspection(digest, "linux", "amd64");
        top.config.entrypoint = vec!["/usr/local/bin/node".to_owned()];
        top.config.cmd = vec!["/app/dist/main.js".to_owned()];
        top.config.user = "node".to_owned();
        let dockerfile = include_str!("../../../../../../extensions/top/Dockerfile");
        assert!(dockerfile.contains("ENTRYPOINT [\"/usr/local/bin/node\"]"));
        assert!(dockerfile.contains("CMD [\"/app/dist/main.js\"]"));
        assert!(dockerfile.contains("USER node"));
        image_from_inspection("top", digest, "amd64", &top).expect("shipped Top process contract");

        for corrupt in ["entrypoint", "command", "user"] {
            let mut candidate = top.clone();
            match corrupt {
                "entrypoint" => candidate.config.entrypoint = vec!["/bin/sh".to_owned()],
                "command" => candidate.config.cmd = vec!["-c".to_owned(), "malicious".to_owned()],
                "user" => candidate.config.user = "root".to_owned(),
                _ => unreachable!(),
            }
            let error = image_from_inspection("top", digest, "amd64", &candidate)
                .expect_err("altered first-party process contract");
            assert!(error.contains("unexpected OCI process configuration"));
            assert!(error.contains("verify the published image"));
            assert!(error.contains("retry workspace provisioning"));
        }

        image_from_inspection("custom", digest, "amd64", &inspection(digest, "linux", "amd64"))
            .expect("custom extension keeps its own image process configuration");
    }
}

#[cfg(test)]
mod halt_tests {
    use hl_extension::{Capability, ExtensionName, Grant, Manifest, Record, Resources, PROTOCOL};

    use super::{Image, Plan, SidecarSpec, Supply as _, Workspace};

    fn plan(socket: std::path::PathBuf) -> Plan {
        let manifest = Manifest {
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            name: ExtensionName::new("checkpoint-sidecar").expect("name"),
            display_name: "Checkpoint sidecar".to_owned(),
            version: "1.0.0".to_owned(),
            protocol: PROTOCOL,
            capabilities: Grant::new([Capability::Interface]),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: Resources::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };
        let record = Record {
            incarnation: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
            name: manifest.name.clone(),
            image_digest: "sha256:offline-checkpoint".to_owned(),
            version: manifest.version.clone(),
            granted: manifest.capabilities.clone(),
            enabled: true,
            installed_at: 1,
            pane_providers: Vec::new(),
            declaration: Some(manifest.clone()),
        };
        let spec = SidecarSpec::new(
            &manifest,
            &record.granted,
            &Image {
                reference: "extension:test".to_owned(),
                digest: record.image_digest.clone(),
                entrypoint: vec!["/extension".to_owned()],
                command: Vec::new(),
                user: "1000:1000".to_owned(),
            },
            socket,
        );
        Plan {
            record,
            manifest,
            spec,
            workspace: "offline-halt".to_owned(),
        }
    }

    #[test]
    fn halting_an_offline_checkpointed_workspace_never_starts_or_changes_its_checkpoint() {
        let root = tempfile::tempdir().expect("state root");
        let name = format!("offline-halt-{}", std::process::id());
        let storage = root.path().join(&name);
        let checkpoint = storage.join("checkpoints/current/MANIFEST");
        std::fs::create_dir_all(checkpoint.parent().expect("checkpoint parent")).expect("checkpoint directory");
        std::fs::write(&checkpoint, b"committed checkpoint inventory").expect("checkpoint fixture");

        let mut config = crate::config::WorkspaceConfig::new(&name, "alpine:3.20", hl_ws::Arch::Amd64);
        config.storage = Some(storage.clone());
        let workspace = Workspace::new(&config);
        let plan = plan(storage.join("extensions/checkpoint-sidecar.sock"));
        let before = crate::workspace_lifecycle::revision();

        workspace.halt(&plan);

        let changes: Vec<_> = crate::workspace_lifecycle::since(before)
            .into_iter()
            .filter(|change| change.workspace == name)
            .collect();
        assert!(
            changes.iter().all(|change| !matches!(
                change.action,
                hl_extension::WorkspaceLifecycleAction::Start | hl_extension::WorkspaceLifecycleAction::Restart
            )),
            "offline halt emitted a start-class lifecycle change: {changes:?}"
        );
        assert_eq!(
            std::fs::read(&checkpoint).expect("checkpoint remains"),
            b"committed checkpoint inventory",
            "offline host teardown changed the published checkpoint inventory"
        );
        assert!(
            !crate::runtime::domain::Domain::new(&config).socket().exists(),
            "offline host teardown created or restored a workspace domain"
        );
        assert!(
            !storage.join("runtime").exists(),
            "offline host teardown entered the domain start path"
        );
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

impl Store {
    fn path() -> PathBuf {
        crate::paths::hl_root().join("workspaces.conf")
    }

    fn create_at(path: PathBuf, workspace: WorkspaceConfig) -> Result<WorkspaceConfiguration, HostError> {
        let mut store = crate::config::WorkspaceStore::load(path).map_err(workspace_io_error)?;
        store.insert(workspace.clone()).map_err(workspace_io_error)?;
        Ok(Self::configuration(&workspace))
    }

    fn configuration(workspace: &WorkspaceConfig) -> WorkspaceConfiguration {
        WorkspaceConfiguration {
            generation: workspace.generation.clone(),
            configuration_revision: workspace.configuration_revision.clone(),
            name: workspace.name.clone(),
            image: workspace.image.clone(),
            architecture: workspace.arch.as_str().to_owned(),
            storage: workspace
                .storage
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            shell: workspace.shell.clone(),
            cpus: workspace.cpus,
            memory_mb: workspace.memory_mb,
            environment: workspace.env.clone(),
            environment_redacted: false,
            mounts: workspace
                .mounts
                .iter()
                .map(|mount| WorkspaceMount {
                    host: mount.host.clone(),
                    container: mount.container.clone(),
                    read_only: mount.ro,
                })
                .collect(),
            docker_socket: workspace.docker_sock,
            scrollback: workspace.scrollback,
            vpn: workspace.vpn.as_ref().map(crate::config::VpnConfig::to_spec),
            execution_lifetime: workspace.execution_lifetime.as_str().to_owned(),
            terminal: WorkspaceTerminal {
                font_family: workspace.terminal.font_family.clone(),
                font_size: workspace.terminal.font_size,
                foreground: workspace.terminal.foreground.clone(),
                background: workspace.terminal.background.clone(),
                cursor_shape: workspace.terminal.cursor_shape.clone(),
                cursor_blink: workspace.terminal.cursor_blink,
            },
        }
    }

    fn configured(value: &WorkspaceConfiguration) -> Result<WorkspaceConfig, HostError> {
        if value.name.trim().is_empty() || value.image.trim().is_empty() {
            return Err(HostError::Conflict("workspace name and image must not be empty".into()));
        }
        let arch = hl_ws::Arch::parse(&value.architecture)
            .ok_or_else(|| HostError::Conflict(format!("unsupported architecture {}", value.architecture)))?;
        let mut workspace = WorkspaceConfig::new(&value.name, &value.image, arch);
        workspace.generation.clone_from(&value.generation);
        if !value.configuration_revision.is_empty() {
            workspace
                .configuration_revision
                .clone_from(&value.configuration_revision);
        }
        workspace.storage = value.storage.as_ref().map(PathBuf::from);
        workspace.shell.clone_from(&value.shell);
        workspace.cpus = value.cpus;
        workspace.memory_mb = value.memory_mb;
        workspace.env.clone_from(&value.environment);
        workspace.mounts = value
            .mounts
            .iter()
            .map(|mount| hl_ws::Mount {
                host: mount.host.clone(),
                container: mount.container.clone(),
                ro: mount.read_only,
            })
            .collect();
        workspace.docker_sock = value.docker_socket;
        workspace.scrollback = value.scrollback;
        workspace.vpn = value.vpn.as_deref().and_then(crate::config::VpnConfig::parse);
        if value.vpn.is_some() && workspace.vpn.is_none() {
            return Err(HostError::Conflict("invalid VPN configuration".into()));
        }
        workspace.execution_lifetime = crate::config::ExecutionLifetime::parse(&value.execution_lifetime)
            .ok_or_else(|| HostError::Conflict("invalid execution lifetime".into()))?;
        workspace.terminal.font_family.clone_from(&value.terminal.font_family);
        workspace.terminal.font_size = value.terminal.font_size;
        workspace.terminal.foreground.clone_from(&value.terminal.foreground);
        workspace.terminal.background.clone_from(&value.terminal.background);
        workspace.terminal.cursor_shape.clone_from(&value.terminal.cursor_shape);
        workspace.terminal.cursor_blink = value.terminal.cursor_blink;
        Ok(workspace)
    }

    fn find(&self, name: &str) -> Result<WorkspaceConfig, HostError> {
        crate::config::WorkspaceStore::load(Self::path())
            .map_err(|error| HostError::Failed(error.to_string()))?
            .get(name)
            .cloned()
            .ok_or_else(|| HostError::Absent(format!("workspace {name}")))
    }

    fn mutable(&self, name: &str) -> Result<WorkspaceConfig, HostError> {
        if name == self.current {
            return Err(HostError::Conflict(
                "an extension cannot stop or delete the workspace hosting it".into(),
            ));
        }
        self.find(name)
    }
}

impl WorkspaceControl for Store {
    fn lifecycle_revision(&self) -> u64 {
        crate::workspace_lifecycle::revision()
    }

    fn lifecycle_since(&self, revision: u64) -> Result<Vec<hl_extension::WorkspaceLifecycleChange>, HostError> {
        Ok(crate::workspace_lifecycle::since(revision))
    }

    fn inspect(&self, name: &str) -> Result<WorkspaceConfiguration, HostError> {
        self.find(name).map(|workspace| Self::configuration(&workspace))
    }

    fn create(&self, configuration: &WorkspaceConfiguration) -> Result<WorkspaceConfiguration, HostError> {
        let workspace = Self::configured(configuration)?;
        Self::create_at(Self::path(), workspace)
    }

    fn update(
        &self,
        name: &str,
        generation: &str,
        configuration_revision: &str,
        configuration: &WorkspaceConfiguration,
    ) -> Result<WorkspaceConfiguration, HostError> {
        let old = self.find(name)?;
        if generation.is_empty() || old.generation != generation || old.configuration_revision != configuration_revision
        {
            return Err(HostError::Conflict(format!(
                "workspace {name} changed; inspect and consent again"
            )));
        }
        if configuration.name != name {
            return Err(HostError::Conflict("renaming a workspace is not supported".into()));
        }
        let mut workspace = Self::configured(configuration)?;
        if Self::running(&old) && workspace.storage != old.storage {
            return Err(HostError::Conflict(
                "workspace storage cannot change while the workspace is running".into(),
            ));
        }
        workspace.generation.clone_from(&old.generation);
        let persisted = crate::config::WorkspaceStore::load(Self::path())
            .and_then(|mut store| store.upsert_if_revision(generation, configuration_revision, workspace))
            .map_err(workspace_io_error)?;
        Ok(Self::configuration(&persisted))
    }

    fn patch_environment(
        &self,
        name: &str,
        generation: &str,
        configuration_revision: &str,
        patch: &hl_extension::port::WorkspaceEnvironmentPatch,
    ) -> Result<hl_extension::port::WorkspaceEnvironmentPatchResult, HostError> {
        let (updated, changed) = crate::config::WorkspaceStore::load(Self::path())
            .and_then(|mut store| {
                store.patch_environment(name, generation, configuration_revision, &patch.set, &patch.remove)
            })
            .map_err(workspace_io_error)?;
        Ok(hl_extension::port::WorkspaceEnvironmentPatchResult {
            generation: updated.generation,
            configuration_revision: updated.configuration_revision,
            changed,
        })
    }

    fn delete(&self, name: &str, generation: &str) -> Result<(), HostError> {
        let workspace = self.mutable(name)?;
        if generation.is_empty() || workspace.generation != generation {
            return Err(HostError::Conflict(format!(
                "workspace {name} changed; inspect and consent again"
            )));
        }
        crate::runtime::domain::Domain::new(&workspace)
            .close(crate::runtime::domain::Close::Kill)
            .map_err(|error| HostError::Failed(error.to_string()))?;
        let removed = crate::config::WorkspaceStore::load(Self::path())
            .and_then(|mut store| store.remove_if_generation(name, generation))
            .map_err(|error| HostError::Failed(error.to_string()))?;
        if removed {
            Ok(())
        } else {
            Err(HostError::Absent(format!("workspace {name}")))
        }
    }

    fn start(&self, name: &str) -> Result<(), HostError> {
        let workspace = self.find(name)?;
        crate::runtime::domain::Domain::new(&workspace)
            .ensure(&workspace)
            .map(|_| ())
            .map_err(|error| HostError::Failed(error.to_string()))
    }

    fn stop(&self, name: &str) -> Result<(), HostError> {
        let workspace = self.mutable(name)?;
        crate::runtime::domain::Domain::new(&workspace)
            .close(crate::runtime::domain::Close::Kill)
            .map_err(|error| HostError::Failed(error.to_string()))?;
        Ok(())
    }

    fn restart(&self, name: &str) -> Result<(), HostError> {
        let workspace = self.mutable(name)?;
        crate::runtime::domain::Domain::new(&workspace)
            .restart(&workspace)
            .map(|_| ())
            .map_err(|error| HostError::Failed(error.to_string()))
    }
}

fn workspace_io_error(error: std::io::Error) -> HostError {
    match error.kind() {
        std::io::ErrorKind::NotFound => HostError::Absent(error.to_string()),
        std::io::ErrorKind::AlreadyExists => HostError::Conflict(error.to_string()),
        _ => HostError::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod workspace_control_tests {
    use hl_extension::port::{HostError, WorkspaceControl as _};
    use hl_extension::ExtensionName;

    use super::{Store, Workspace};

    #[test]
    fn concurrent_workspace_creation_never_replaces_the_winning_identity() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("workspaces.conf");
        let first = crate::config::WorkspaceConfig::new("database", "postgres:17", hl_ws::Arch::Amd64);
        let first_generation = first.generation.clone();
        Store::create_at(path.clone(), first).expect("first creation");

        let second = crate::config::WorkspaceConfig::new("database", "malicious:latest", hl_ws::Arch::Amd64);
        assert!(matches!(Store::create_at(path.clone(), second), Err(HostError::Conflict(_))));
        let persisted = crate::config::WorkspaceStore::load(path).expect("persisted workspace");
        let database = persisted.get("database").expect("winning workspace");
        assert_eq!(database.image, "postgres:17");
        assert_eq!(database.generation, first_generation);
    }

    #[test]
    fn each_extension_gets_one_private_socket_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut configuration = crate::config::WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        configuration.storage = Some(temporary.path().join("workspace"));
        let top = ExtensionName::new("top").expect("name");
        let storybook = ExtensionName::new("storybook").expect("name");

        assert_eq!(
            Workspace::extension(&configuration, &top).socket(&top),
            temporary.path().join("workspace/extensions/top/extension.sock")
        );
        assert_eq!(
            Workspace::extension(&configuration, &storybook).socket(&storybook),
            temporary.path().join("workspace/extensions/storybook/extension.sock")
        );
    }

    #[test]
    fn extension_configuration_round_trips_every_persisted_core_field() {
        let mut workspace = crate::config::WorkspaceConfig::new("other", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some("/var/tmp/other".into());
        workspace.shell = Some("/bin/bash -l".into());
        workspace.cpus = Some(4);
        workspace.memory_mb = Some(4096);
        workspace.env = vec![("MODE".into(), "dev".into())];
        workspace.mounts = vec![hl_ws::Mount {
            host: "/source".into(),
            container: "/workspace".into(),
            ro: true,
        }];
        workspace.docker_sock = false;
        workspace.scrollback = None;
        workspace.vpn = Some(crate::config::VpnConfig::socks5("127.0.0.1:1080"));
        workspace.execution_lifetime = crate::config::ExecutionLifetime::Live;
        workspace.terminal.font_family = Some("Mono".into());
        workspace.terminal.font_size = Some(13);
        workspace.terminal.cursor_blink = Some(false);

        let carried = Store::configuration(&workspace);
        let restored = Store::configured(&carried).expect("valid configuration");
        workspace.generation.clone_from(&restored.generation);
        assert_eq!(restored, workspace);
    }

    #[test]
    fn malformed_architecture_and_vpn_are_rejected_before_persistence() {
        let workspace = crate::config::WorkspaceConfig::new("other", "alpine", hl_ws::Arch::Arm64);
        let mut carried = Store::configuration(&workspace);
        carried.architecture = "mips".into();
        assert!(Store::configured(&carried).is_err());
        carried.architecture = "arm64".into();
        carried.vpn = Some("not a proxy".into());
        assert!(Store::configured(&carried).is_err());
    }

    #[test]
    fn lifecycle_revisions_are_stable_across_store_instances() {
        let first = Store { current: "one".into() };
        let before = first.lifecycle_revision();
        let created = format!("ledger-created-{}", std::process::id());
        let started = format!("ledger-started-{}", std::process::id());
        crate::workspace_lifecycle::changed(&created, hl_extension::WorkspaceLifecycleAction::Create);
        crate::workspace_lifecycle::changed(&started, hl_extension::WorkspaceLifecycleAction::Start);
        let second = Store { current: "two".into() };
        let selected = |store: &Store| {
            store
                .lifecycle_since(before)
                .expect("lifecycle")
                .into_iter()
                .filter(|change| change.workspace == created || change.workspace == started)
                .collect::<Vec<_>>()
        };
        let changes = selected(&second);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].workspace, created);
        assert_eq!(changes[1].workspace, started);
        assert!(changes[0].revision < changes[1].revision);
        let third = Store {
            current: "three".into(),
        };
        assert_eq!(
            selected(&third),
            changes,
            "store instances observe the same exact revisions"
        );
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
