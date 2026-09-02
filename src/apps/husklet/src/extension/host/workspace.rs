//! The real supply: one workspace, its records, and its container daemon.
//!
//! Everything in this file needs a running workspace, which is why it is the
//! one part of the host the suite cannot drive. It is kept apart from the
//! orchestration for exactly that reason: what can be tested and what cannot
//! are not mixed in one file.

use std::path::PathBuf;
use std::sync::Arc;

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

    /// Deletes the sidecar owned by one still-installed record. The record is
    /// deliberately read before its caller forgets it: its digest, grant,
    /// limits, and socket are the authority for exact ownership.
    pub fn remove_extension(workspace: &WorkspaceConfig, name: &ExtensionName) -> Result<(), String> {
        let supply = Self::extension(workspace, name);
        let storage = hl_ws::storage::Directory::open(supply.root()).map_err(|error| error.to_string())?;
        let records = Records::open(storage).map_err(|fault| fault.to_string())?;
        let record = records
            .all()
            .map_err(|fault| fault.to_string())?
            .into_iter()
            .find(|record| record.name == *name)
            .ok_or_else(|| format!("extension {name} is not installed"))?;
        let manifest = described(&record);
        // Entrypoint and user do not participate in the ownership signature;
        // no image lookup is needed merely to delete an already-created sidecar.
        let image = Image {
            reference: record.image_digest.clone(),
            digest: record.image_digest.clone(),
            entrypoint: Vec::new(),
            user: String::new(),
        };
        let spec = SidecarSpec::new(&manifest, &record.granted, &image, supply.socket(name));
        Sidecar::new(supply.bridge()?)
            .remove_owned(&spec)
            .map_err(|error| error.to_string())
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
        conversation.with_events(self.events.clone());
        let extensions = Extensions::open(&self.config).map_err(|error| error.to_string())?;
        conversation.with_extension_events(extensions.extension_events());
        let console = Console;
        let terminal: &dyn TerminalSurface = self.terminal.as_deref().unwrap_or(&console);
        let store = Store {
            current: self.config.name.clone(),
        };
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
        };
        conversation.serve(&services).map_err(|fault| fault.to_string())
    }

    fn halt(&self, plan: &Plan) {
        let Ok(bridge) = self.bridge() else {
            return;
        };
        if let Err(error) = Sidecar::new(bridge).stop_owned(&plan.spec) {
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

impl Store {
    fn path() -> PathBuf {
        crate::paths::hl_root().join("workspaces.conf")
    }

    fn configuration(workspace: &WorkspaceConfig) -> WorkspaceConfiguration {
        WorkspaceConfiguration {
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
        let mut store =
            crate::config::WorkspaceStore::load(Self::path()).map_err(|error| HostError::Failed(error.to_string()))?;
        if store.get(&workspace.name).is_some() {
            return Err(HostError::Conflict(format!(
                "workspace {} already exists",
                workspace.name
            )));
        }
        store
            .upsert(workspace.clone())
            .map_err(|error| HostError::Failed(error.to_string()))?;
        Ok(Self::configuration(&workspace))
    }

    fn update(&self, name: &str, configuration: &WorkspaceConfiguration) -> Result<WorkspaceConfiguration, HostError> {
        let old = self.find(name)?;
        if configuration.name != name {
            return Err(HostError::Conflict("renaming a workspace is not supported".into()));
        }
        if Self::running(&old) {
            return Err(HostError::Conflict(
                "stop the workspace before changing its configuration".into(),
            ));
        }
        let workspace = Self::configured(configuration)?;
        crate::config::WorkspaceStore::load(Self::path())
            .and_then(|mut store| store.upsert(workspace.clone()))
            .map_err(|error| HostError::Failed(error.to_string()))?;
        Ok(Self::configuration(&workspace))
    }

    fn delete(&self, name: &str) -> Result<(), HostError> {
        let workspace = self.mutable(name)?;
        crate::runtime::domain::Domain::new(&workspace)
            .close(crate::runtime::domain::Close::Kill)
            .map_err(|error| HostError::Failed(error.to_string()))?;
        let removed = crate::config::WorkspaceStore::load(Self::path())
            .and_then(|mut store| store.remove(name))
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

#[cfg(test)]
mod workspace_control_tests {
    use hl_extension::port::WorkspaceControl as _;

    use super::Store;

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
    fn lifecycle_ledger_is_bounded_and_revisions_are_stable_across_store_instances() {
        let first = Store { current: "one".into() };
        let before = first.lifecycle_revision();
        crate::workspace_lifecycle::changed("created", hl_extension::WorkspaceLifecycleAction::Create);
        crate::workspace_lifecycle::changed("started", hl_extension::WorkspaceLifecycleAction::Start);
        let second = Store { current: "two".into() };
        let changes = second.lifecycle_since(before).expect("lifecycle");
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].workspace, "created");
        assert_eq!(changes[1].workspace, "started");
        assert!(changes[0].revision < changes[1].revision);
        assert_eq!(second.lifecycle_revision(), changes[1].revision);

        let overflow_start = second.lifecycle_revision();
        for index in 0..258 {
            crate::workspace_lifecycle::changed(
                &format!("overflow-{index}"),
                hl_extension::WorkspaceLifecycleAction::Update,
            );
        }
        let bounded = second.lifecycle_since(overflow_start).expect("bounded lifecycle");
        assert_eq!(bounded.len(), 256);
        assert_eq!(bounded[0].coalesced, 2);
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
