//! Dispatch and enforcement, driven entirely by in-memory ports.
//!
//! No container runtime, no socket, no toolkit. That this suite runs at all is
//! the evidence that the ports-and-adapters split is real rather than
//! decorative: if the protocol had reached for a service directly, none of this
//! could be written.

use std::cell::{Cell, RefCell};

use hl_extension::port::{
    ContainerControl, ContainerInventory, ContainerOutput, ContainerSummary, Division, Entry, ExecutionSummary,
    ExtensionAcquisitionJob, ExtensionAcquisitionStatus, ExtensionStore, ExtensionSummary, GridSize, HostError,
    ImageDetails, ImagePruneResult, ImageStore, ImageSummary, Occupant, PaneSemanticAction, PaneSemanticTree,
    PaneSummary, PaneText, ProcessList, SemanticActionKind, SemanticNode, TabSummary, TerminalSurface,
    TerminalTopology, WorkspaceFiles, WorkspaceInventory, WorkspaceState,
};
use hl_extension::{
    Authority, Capability, ExtensionName, Failure, Grant, RelativePath, Reply, Request, Services, Session, Topic,
    WorkspaceConfiguration, WorkspaceInfo, WorkspaceTerminal,
};

/// Records what was actually reached, so a refusal that still touched a service
/// would be visible rather than silent.
#[derive(Debug, Default)]
struct Ledger {
    reached: RefCell<Vec<&'static str>>,
}

impl Ledger {
    fn note(&self, what: &'static str) {
        self.reached.borrow_mut().push(what);
    }

    fn reached(&self) -> Vec<&'static str> {
        self.reached.borrow().clone()
    }
}

struct Host {
    ledger: Ledger,
    cancelled_revision: Cell<Option<u64>>,
}
impl hl_extension::port::VolumeStore for Host {
    fn list(&self) -> Result<Vec<hl_extension::port::VolumeSummary>, HostError> {
        self.ledger.note("volumes.list");
        Ok(vec![hl_extension::port::VolumeSummary {
            name: "cache".into(),
            driver: "local".into(),
            generation: "a".repeat(32),
        }])
    }
    fn inspect(&self, name: &str) -> Result<hl_extension::port::VolumeSummary, HostError> {
        self.ledger.note("volumes.inspect");
        Ok(hl_extension::port::VolumeSummary {
            name: name.into(),
            driver: "local".into(),
            generation: "a".repeat(32),
        })
    }
    fn create(&self, name: &str) -> Result<hl_extension::port::VolumeSummary, HostError> {
        self.ledger.note("volumes.create");
        Ok(hl_extension::port::VolumeSummary {
            name: name.into(),
            driver: "local".into(),
            generation: "a".repeat(32),
        })
    }
    fn remove(&self, _name: &str, _generation: &str) -> Result<(), HostError> {
        self.ledger.note("volumes.remove");
        Ok(())
    }
}
impl hl_extension::port::NetworkStore for Host {
    fn list(&self) -> Result<Vec<hl_extension::port::NetworkSummary>, HostError> {
        self.ledger.note("networks.list");
        Ok(vec![hl_extension::port::NetworkSummary {
            id: "a".repeat(32),
            name: "private".into(),
            driver: "bridge".into(),
            scope: "local".into(),
        }])
    }
    fn inspect(&self, reference: &str) -> Result<hl_extension::port::NetworkSummary, HostError> {
        self.ledger.note("networks.inspect");
        Ok(hl_extension::port::NetworkSummary {
            id: "a".repeat(32),
            name: reference.into(),
            driver: "bridge".into(),
            scope: "local".into(),
        })
    }
    fn create(&self, _name: &str) -> Result<String, HostError> {
        self.ledger.note("networks.create");
        Ok("a".repeat(32))
    }
    fn remove(&self, _reference: &str) -> Result<(), HostError> {
        self.ledger.note("networks.remove");
        Ok(())
    }
    fn connect(&self, _reference: &str, _container: &str) -> Result<(), HostError> {
        self.ledger.note("networks.connect");
        Ok(())
    }
    fn connect_with_aliases(&self, _reference: &str, _container: &str, _aliases: &[String]) -> Result<(), HostError> {
        self.ledger.note("networks.connect");
        Ok(())
    }
    fn disconnect(&self, _reference: &str, _container: &str) -> Result<(), HostError> {
        self.ledger.note("networks.disconnect");
        Ok(())
    }
}

impl Host {
    fn new() -> Self {
        Self {
            ledger: Ledger::default(),
            cancelled_revision: Cell::new(None),
        }
    }

    fn container() -> ContainerSummary {
        ContainerSummary {
            id: "c1".into(),
            name: "api".into(),
            image: "husklet/api:1".into(),
            state: "running".into(),
            created: 0,
        }
    }
}

impl ContainerInventory for Host {
    fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
        self.ledger.note("containers.list");
        Ok(vec![Self::container()])
    }

    fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
        self.ledger.note("containers.inspect");
        if id == "c1" {
            return Ok(Self::container());
        }
        Err(HostError::Absent(id.into()))
    }

    fn processes(&self, _id: &str) -> Result<ProcessList, HostError> {
        self.ledger.note("containers.processes");
        Ok(ProcessList {
            container_id: "c".repeat(64),
            titles: vec!["PID".into(), "CMD".into()],
            processes: vec![vec!["7".into(), "server".into()]],
            observed_at_ms: 1_700_000_000_000,
            scope: hl_extension::port::ProcessScope::Initial,
            pid_identity: hl_extension::port::ProcessPidIdentity::Snapshot,
            truncated: false,
        })
    }

    fn logs(&self, _id: &str, _stdout: bool, _stderr: bool) -> Result<ContainerOutput, HostError> {
        self.ledger.note("containers.logs");
        Ok(ContainerOutput {
            stdout: b"ready\n".to_vec(),
            stderr: Vec::new(),
            truncated: false,
            stdout_truncated: false,
            stderr_truncated: false,
            eof: false,
        })
    }

    fn execution(&self, id: &str) -> Result<ExecutionSummary, HostError> {
        self.ledger.note("executions.inspect");
        Ok(ExecutionSummary {
            id: id.into(),
            container_id: "c1".into(),
            running: true,
            exit_code: 0,
            pid: 8,
            command: vec!["worker".into()],
            user: "root".into(),
        })
    }
    fn executions(&self) -> Result<hl_extension::port::ExecutionList, HostError> {
        self.ledger.note("executions.list");
        Ok(hl_extension::port::ExecutionList {
            executions: vec![self.execution("e1")?],
            truncated: false,
        })
    }
    fn execution_logs(&self, _id: &str, _stdout: bool, _stderr: bool) -> Result<ContainerOutput, HostError> {
        self.ledger.note("executions.logs");
        Ok(ContainerOutput {
            stdout: b"exec out\n".to_vec(),
            stderr: b"exec err\n".to_vec(),
            truncated: false,
            stdout_truncated: false,
            stderr_truncated: false,
            eof: true,
        })
    }

    fn execution_wait(&self, id: &str, _timeout_ms: u32) -> Result<ExecutionSummary, HostError> {
        self.ledger.note("executions.wait");
        Ok(ExecutionSummary {
            id: id.into(),
            container_id: "c1".into(),
            running: false,
            exit_code: 17,
            pid: 0,
            command: vec!["worker".into()],
            user: "root".into(),
        })
    }
}

impl ContainerControl for Host {
    fn create(&self, _image: &str, name: &str) -> Result<String, HostError> {
        self.ledger.note("containers.create");
        Ok(format!("id-{name}"))
    }

    fn create_spec(&self, spec: &hl_extension::port::ContainerCreateSpec) -> Result<String, HostError> {
        self.ledger.note("containers.create_spec");
        Ok(format!("id-{}", spec.name))
    }

    fn start(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("containers.start");
        Ok(())
    }

    fn stop(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("containers.stop");
        Ok(())
    }

    fn remove(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("containers.remove");
        Ok(())
    }

    fn pause(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("containers.pause");
        Ok(())
    }

    fn unpause(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("containers.unpause");
        Ok(())
    }

    fn restart(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("containers.restart");
        Ok(())
    }

    fn rename(&self, _id: &str, _name: &str) -> Result<(), HostError> {
        self.ledger.note("containers.rename");
        Ok(())
    }

    fn kill(&self, _id: &str, _signal: &str) -> Result<(), HostError> {
        self.ledger.note("containers.kill");
        Ok(())
    }

    fn execution_kill(&self, _id: &str, _signal: &str) -> Result<(), HostError> {
        self.ledger.note("executions.kill");
        Ok(())
    }
    fn execution_remove(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("executions.remove");
        Ok(())
    }

    fn execute(
        &self,
        _id: &str,
        _command: &[String],
        _user: Option<&str>,
        _working_directory: Option<&str>,
    ) -> Result<String, HostError> {
        self.ledger.note("containers.exec");
        Ok("e1".into())
    }
}

impl ImageStore for Host {
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        self.ledger.note("images.list");
        Ok(Vec::new())
    }

    fn pull(&self, reference: &str) -> Result<ImageSummary, HostError> {
        self.ledger.note("images.pull");
        Ok(ImageSummary {
            id: "i1".into(),
            reference: reference.into(),
            size: 1,
            created: 0,
        })
    }

    fn inspect(&self, reference: &str) -> Result<ImageDetails, HostError> {
        self.ledger.note("images.inspect");
        Ok(ImageDetails {
            id: reference.into(),
            references: vec![reference.into()],
            created: String::new(),
            size: 1,
            os: "linux".into(),
            architecture: "amd64".into(),
            entrypoint: Vec::new(),
            command: Vec::new(),
            working_directory: String::new(),
            user: String::new(),
        })
    }

    fn remove(&self, _reference: &str) -> Result<(), HostError> {
        self.ledger.note("images.remove");
        Ok(())
    }

    fn prune(&self) -> Result<ImagePruneResult, HostError> {
        self.ledger.note("images.prune");
        Ok(ImagePruneResult {
            deleted: 2,
            space_reclaimed: 7,
        })
    }
}

impl TerminalSurface for Host {
    fn attach_container(&self, _id: &str, _command: &[String]) -> Result<String, HostError> {
        self.ledger.note("terminal.attach_container");
        Ok("attached-pane".into())
    }
    fn pane_inventory(&self) -> Result<hl_extension::port::PaneInventory, HostError> {
        self.ledger.note("terminal.pane_inventory");
        Ok(hl_extension::port::PaneInventory {
            panes: vec![hl_extension::port::InspectablePane {
                slot: "workspace".into(),
                generation: 0,
                revision: 0,
                kind: hl_extension::port::PaneKind::Native,
                provider: None,
                tab: None,
                title: Some("Workspace".into()),
                focused: false,
            }],
            truncated: false,
        })
    }

    fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
        self.ledger.note("terminal.tabs");
        Ok(vec![TabSummary {
            id: "t1".into(),
            title: "shell".into(),
            panes: vec![PaneSummary {
                slot: "s1".into(),
                working_directory: Some("/root".into()),
                command: Some("bash".into()),
                occupant: Occupant::Terminal,
                provider: None,
            }],
        }])
    }

    fn topology(&self) -> Result<TerminalTopology, HostError> {
        self.ledger.note("terminal.topology");
        Ok(TerminalTopology {
            active_tab: Some("t1".into()),
            tabs: Vec::new(),
        })
    }

    fn open_tab(&self, title: &str) -> Result<String, HostError> {
        self.ledger.note("terminal.open_tab");
        Ok(format!("tab-{title}"))
    }

    fn split(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        self.ledger.note("terminal.split");
        Ok("s2".into())
    }

    fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
        self.ledger.note("terminal.spawn");
        Ok(())
    }
    fn read(&self, slot: &str, lines: usize) -> Result<PaneText, HostError> {
        if slot == "oversized" {
            return Ok(PaneText {
                slot: slot.into(),
                generation: 0,
                revision: 0,
                columns: 80,
                rows: 24,
                lines: vec!["old".repeat(hl_extension::port::PANE_TEXT_BYTES / 3), "new".into()],
                cursor_column: 0,
                cursor_row: 0,
                truncated: false,
            });
        }
        Ok(PaneText {
            slot: slot.into(),
            generation: 0,
            revision: 0,
            columns: 120,
            rows: 40,
            lines: vec![format!("at most {lines}")],
            cursor_column: 12,
            cursor_row: 3,
            truncated: true,
        })
    }

    fn semantics(&self, slot: &str) -> Result<PaneSemanticTree, HostError> {
        self.ledger.note("terminal.semantics");
        Ok(PaneSemanticTree {
            slot: slot.into(),
            generation: 0,
            revision: 4,
            truncated: false,
            root: SemanticNode {
                id: 0,
                role: "column".into(),
                label: None,
                value: None,
                disabled: false,
                destructive: false,
                actions: vec![],
                children: vec![],
            },
        })
    }

    fn semantic_action(&self, _slot: &str, _action: &PaneSemanticAction) -> Result<(), HostError> {
        self.ledger.note("terminal.semantic_action");
        Ok(())
    }

    fn semantic_requirement(&self, slot: &str, node: u64) -> Result<Capability, HostError> {
        match (slot, node) {
            ("workspace", 98) => Ok(Capability::WorkspaceControl),
            ("workspace", 99) => Ok(Capability::ExtensionControl),
            _ => Ok(Capability::PaneSemanticControl),
        }
    }

    fn write(&self, _slot: &str, _generation: u64, _revision: u64, _contents: &[u8]) -> Result<(), HostError> {
        self.ledger.note("terminal.write");
        Ok(())
    }

    fn resize_grid(&self, _slot: &str, _grid: GridSize) -> Result<(), HostError> {
        self.ledger.note("terminal.resize_grid");
        Ok(())
    }

    fn retitle(&self, _slot: &str, _title: &str) -> Result<(), HostError> {
        self.ledger.note("terminal.retitle");
        Ok(())
    }

    fn close(&self, _slot: &str) -> Result<(), HostError> {
        self.ledger.note("terminal.close");
        Ok(())
    }

    fn focus(&self, _slot: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn ratio(&self, _slot: &str, _ratio: f64) -> Result<(), HostError> {
        Ok(())
    }

    fn switch_occupant(
        &self,
        _slot: &str,
        _generation: u64,
        _target: &hl_extension::port::PaneOccupantTarget,
    ) -> Result<(), HostError> {
        self.ledger.note("terminal.switch_occupant");
        Ok(())
    }

    fn surface(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Ok("s3".into())
    }
}

#[test]
fn terminal_screen_bytes_are_bounded_before_the_reply_is_encoded() {
    let host = Host::new();
    let reply = session(&[Capability::TerminalOutput], &[])
        .dispatch(
            &Request::TerminalReadPane {
                slot: "oversized".into(),
                lines: None,
            },
            &services(&host),
        )
        .expect("bounded screen");
    let Reply::Text(text) = reply else {
        panic!("wrong reply")
    };
    assert!(text.truncated);
    assert_eq!(text.lines, vec!["new"]);
    assert_eq!((text.cursor_column, text.cursor_row), (0, 0));
    assert_eq!((text.columns, text.rows), (80, 24));
}

#[test]
fn pane_semantic_read_and_control_are_separately_granted() {
    let host = Host::new();
    let read = Request::PaneSemanticRead { slot: "s1".into() };
    let action = Request::PaneSemanticAction {
        slot: "s1".into(),
        action: PaneSemanticAction {
            generation: 0,
            revision: 4,
            node: 2,
            action: SemanticActionKind::Invoke,
            value: None,
        },
    };
    assert!(matches!(
        session(&[Capability::PaneSemanticRead], &[]).dispatch(&read, &services(&host)),
        Ok(Reply::Semantics(_))
    ));
    assert!(session(&[Capability::PaneSemanticRead], &[])
        .dispatch(&action, &services(&host))
        .is_err());
    session(&[Capability::PaneSemanticControl], &[])
        .dispatch(&action, &services(&host))
        .expect("controlled");
    assert_eq!(
        host.ledger.reached(),
        vec!["terminal.semantics", "terminal.semantic_action"]
    );
}

#[test]
fn pane_discovery_requires_observation_without_content_authority() {
    let host = Host::new();
    assert!(session(&[], &[])
        .dispatch(&Request::PaneList, &services(&host))
        .is_err());
    let reply = session(&[Capability::PaneObserve], &[])
        .dispatch(&Request::PaneList, &services(&host))
        .expect("pane observation grants bounded discovery");
    let Reply::Panes(inventory) = reply else {
        panic!("wrong reply")
    };
    assert_eq!(inventory.panes[0].slot, "workspace");
    assert_eq!(host.ledger.reached(), vec!["terminal.pane_inventory"]);
}

#[test]
fn native_semantic_actions_require_the_underlying_domain_grant() {
    let host = Host::new();
    let action = |node| Request::PaneSemanticAction {
        slot: "workspace".into(),
        action: PaneSemanticAction {
            generation: 0,
            revision: 1,
            node,
            action: SemanticActionKind::Invoke,
            value: None,
        },
    };
    for node in [98, 99] {
        let denied = session(&[Capability::PaneSemanticControl], &[]).dispatch(&action(node), &services(&host));
        assert!(matches!(denied, Err(Failure::Denied { .. })));
    }
    assert!(host.ledger.reached().is_empty(), "denial must precede the callback");
    session(&[Capability::PaneSemanticControl, Capability::ExtensionControl], &[])
        .dispatch(&action(99), &services(&host))
        .expect("explicit lifecycle grant");
    session(&[Capability::PaneSemanticControl, Capability::WorkspaceControl], &[])
        .dispatch(&action(98), &services(&host))
        .expect("explicit workspace grant");
    assert_eq!(
        host.ledger.reached(),
        vec!["terminal.semantic_action", "terminal.semantic_action"]
    );
}

impl WorkspaceInventory for Host {
    fn workspaces(&self) -> Result<Vec<WorkspaceState>, HostError> {
        Ok(vec![WorkspaceState {
            name: "dev".into(),
            architecture: "arm64".into(),
            image: "alpine:3.20".into(),
            running: true,
            current: true,
        }])
    }
}

impl hl_extension::port::WorkspaceControl for Host {
    fn inspect(&self, _name: &str) -> Result<WorkspaceConfiguration, HostError> {
        self.ledger.note("workspace.inspect");
        Ok(workspace_configuration())
    }
    fn create(&self, configuration: &WorkspaceConfiguration) -> Result<WorkspaceConfiguration, HostError> {
        self.ledger.note("workspace.create");
        Ok(configuration.clone())
    }
    fn adopt(&self, configuration: &WorkspaceConfiguration) -> Result<WorkspaceConfiguration, HostError> {
        self.ledger.note("workspace.adopt");
        let mut adopted = configuration.clone();
        adopted.generation = "0123456789abcdef0123456789abcdef".into();
        Ok(adopted)
    }
    fn update(
        &self,
        _name: &str,
        _generation: &str,
        configuration: &WorkspaceConfiguration,
    ) -> Result<WorkspaceConfiguration, HostError> {
        self.ledger.note("workspace.update");
        Ok(configuration.clone())
    }
    fn delete(&self, _name: &str, _generation: &str) -> Result<(), HostError> {
        self.ledger.note("workspace.delete");
        Ok(())
    }
    fn start(&self, _name: &str) -> Result<(), HostError> {
        self.ledger.note("workspace.start");
        Ok(())
    }
    fn stop(&self, _name: &str) -> Result<(), HostError> {
        self.ledger.note("workspace.stop");
        Ok(())
    }
    fn restart(&self, _name: &str) -> Result<(), HostError> {
        self.ledger.note("workspace.restart");
        Ok(())
    }
}

impl WorkspaceFiles for Host {
    fn list(&self, path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        self.ledger.note("files.list");
        Ok(vec![Entry {
            path: path.clone(),
            directory: true,
            size: 0,
        }])
    }

    fn read(&self, _path: &RelativePath) -> Result<Vec<u8>, HostError> {
        self.ledger.note("files.read");
        Ok(b"contents".to_vec())
    }
    fn stat(&self, path: &RelativePath) -> Result<Entry, HostError> {
        self.ledger.note("files.stat");
        Ok(Entry {
            path: path.clone(),
            directory: false,
            size: 7,
        })
    }

    fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
        self.ledger.note("files.write");
        Ok(())
    }

    fn mkdir(&self, _path: &RelativePath) -> Result<(), HostError> {
        self.ledger.note("files.mkdir");
        Ok(())
    }

    fn rename(&self, _from: &RelativePath, _to: &RelativePath) -> Result<(), HostError> {
        self.ledger.note("files.rename");
        Ok(())
    }

    fn remove(&self, _path: &RelativePath) -> Result<(), HostError> {
        self.ledger.note("files.remove");
        Ok(())
    }
}

impl ExtensionStore for Host {
    fn list(&self) -> Result<Vec<ExtensionSummary>, HostError> {
        self.ledger.note("extensions.list");
        Ok(vec![ExtensionSummary {
            name: "sample".into(),
            image_digest: "sha256:abc".into(),
            status: "duty".into(),
        }])
    }
    fn inspect(&self, name: &str) -> Result<ExtensionSummary, HostError> {
        self.ledger.note("extensions.inspect");
        Ok(ExtensionSummary {
            name: name.into(),
            image_digest: "sha256:abc".into(),
            status: "duty".into(),
        })
    }
    fn enable(&self, _name: &str, _image_digest: &str) -> Result<(), HostError> {
        self.ledger.note("extensions.enable");
        Ok(())
    }
    fn disable(&self, _name: &str, _image_digest: &str) -> Result<(), HostError> {
        self.ledger.note("extensions.disable");
        Ok(())
    }
    fn remove(&self, _name: &str, _image_digest: &str) -> Result<(), HostError> {
        self.ledger.note("extensions.remove");
        Ok(())
    }
    fn acquisition_start(&self, _reference: &str) -> Result<ExtensionAcquisitionJob, HostError> {
        self.ledger.note("extensions.acquisition_start");
        Ok(ExtensionAcquisitionJob { job: "job-1".into() })
    }
    fn acquisition_status(&self, job: &str) -> Result<ExtensionAcquisitionStatus, HostError> {
        self.ledger.note("extensions.acquisition_status");
        Ok(ExtensionAcquisitionStatus {
            job: job.into(),
            reference: "registry/example:1".into(),
            revision: 7,
            state: "ready".into(),
            progress: None,
            candidate: None,
            error: None,
        })
    }
    fn acquisition_cancel(&self, _job: &str, revision: u64) -> Result<(), HostError> {
        self.ledger.note("extensions.acquisition_cancel");
        self.cancelled_revision.set(Some(revision));
        Ok(())
    }
    fn install(&self, job: &str, _revision: u64, _granted: &Grant) -> Result<ExtensionSummary, HostError> {
        self.ledger.note("extensions.install");
        ExtensionStore::inspect(self, job)
    }
    fn update(&self, job: &str, _revision: u64, _granted: &Grant) -> Result<ExtensionSummary, HostError> {
        self.ledger.note("extensions.update");
        ExtensionStore::inspect(self, job)
    }
}

fn services(host: &Host) -> Services<'_> {
    Services {
        workspace: WorkspaceInfo {
            name: "dev".into(),
            architecture: "arm64".into(),
            image: "alpine:3.20".into(),
        },
        workspaces: host,
        workspace_control: host,
        extensions: host,
        containers: host,
        control: host,
        images: host,
        volumes: host,
        networks: host,
        terminal: host,
        files: host,
    }
}

fn session(capabilities: &[Capability], roots: &[&str]) -> Session {
    Session::new(Authority::new(
        ExtensionName::new("sample").expect("name"),
        Grant::new(capabilities.iter().copied()),
        roots
            .iter()
            .map(|root| RelativePath::new(*root).expect("root"))
            .collect(),
    ))
}

fn path(value: &str) -> RelativePath {
    RelativePath::new(value).expect("path")
}

fn workspace_configuration() -> WorkspaceConfiguration {
    WorkspaceConfiguration {
        generation: "0123456789abcdef0123456789abcdef".into(),
        name: "other".into(),
        image: "alpine:3.20".into(),
        architecture: "arm64".into(),
        storage: None,
        shell: None,
        cpus: None,
        memory_mb: None,
        environment: Vec::new(),
        mounts: Vec::new(),
        docker_socket: true,
        scrollback: Some(100_000),
        vpn: None,
        execution_lifetime: "persisted".into(),
        terminal: WorkspaceTerminal::default(),
    }
}

/// Every call, paired with the capability that must permit it.
fn calls() -> Vec<(Request, Capability)> {
    vec![
        (Request::WorkspaceInfo, Capability::WorkspaceRead),
        (Request::WorkspaceList, Capability::WorkspaceRead),
        (
            Request::WorkspaceInspect { name: "other".into() },
            Capability::WorkspaceRead,
        ),
        (
            Request::WorkspaceCreate {
                configuration: workspace_configuration(),
            },
            Capability::WorkspaceControl,
        ),
        (
            Request::WorkspaceAdopt {
                configuration: WorkspaceConfiguration {
                    generation: String::new(),
                    ..workspace_configuration()
                },
            },
            Capability::WorkspaceControl,
        ),
        (
            Request::WorkspaceUpdate {
                name: "other".into(),
                generation: "0123456789abcdef0123456789abcdef".into(),
                configuration: workspace_configuration(),
            },
            Capability::WorkspaceControl,
        ),
        (
            Request::WorkspaceDelete {
                name: "other".into(),
                generation: "0123456789abcdef0123456789abcdef".into(),
            },
            Capability::WorkspaceControl,
        ),
        (
            Request::WorkspaceStart { name: "other".into() },
            Capability::WorkspaceControl,
        ),
        (
            Request::WorkspaceStop { name: "other".into() },
            Capability::WorkspaceControl,
        ),
        (
            Request::WorkspaceRestart { name: "other".into() },
            Capability::WorkspaceControl,
        ),
        (Request::ExtensionList, Capability::ExtensionRead),
        (
            Request::ExtensionInspect { name: "sample".into() },
            Capability::ExtensionRead,
        ),
        (
            Request::ExtensionEnable {
                name: "sample".into(),
                image_digest: format!("sha256:{}", "a".repeat(64)),
            },
            Capability::ExtensionControl,
        ),
        (
            Request::ExtensionDisable {
                name: "sample".into(),
                image_digest: format!("sha256:{}", "a".repeat(64)),
            },
            Capability::ExtensionControl,
        ),
        (
            Request::ExtensionRemove {
                name: "sample".into(),
                image_digest: format!("sha256:{}", "a".repeat(64)),
            },
            Capability::ExtensionControl,
        ),
        (
            Request::ExtensionAcquisitionStart {
                reference: "registry/example:1".into(),
            },
            Capability::ExtensionInstall,
        ),
        (
            Request::ExtensionAcquisitionStatus { job: "job-1".into() },
            Capability::ExtensionInstall,
        ),
        (
            Request::ExtensionAcquisitionCancel {
                job: "job-1".into(),
                revision: 7,
            },
            Capability::ExtensionInstall,
        ),
        (
            Request::ExtensionInstall {
                job: "job-1".into(),
                revision: 7,
                granted: Grant::new([Capability::Interface]),
            },
            Capability::ExtensionInstall,
        ),
        (
            Request::ExtensionUpdate {
                job: "job-1".into(),
                revision: 7,
                granted: Grant::new([Capability::Interface]),
            },
            Capability::ExtensionInstall,
        ),
        (Request::ContainerList, Capability::ContainerRead),
        (Request::ContainerInspect { id: "c1".into() }, Capability::ContainerRead),
        (
            Request::ContainerProcesses { id: "c1".into() },
            Capability::ContainerRead,
        ),
        (
            Request::ContainerLogs {
                id: "c1".into(),
                stdout: true,
                stderr: true,
            },
            Capability::ContainerRead,
        ),
        (Request::ExecutionInspect { id: "e".repeat(32) }, Capability::ContainerRead),
        (Request::ExecutionList, Capability::ContainerRead),
        (
            Request::ExecutionLogs {
                id: "e".repeat(32),
                stdout: true,
                stderr: true,
            },
            Capability::ContainerRead,
        ),
        (
            Request::ExecutionWait {
                id: "e".repeat(32),
                timeout_ms: 500,
            },
            Capability::ContainerRead,
        ),
        (
            Request::ContainerCreate {
                spec: hl_extension::port::ContainerCreateSpec {
                    image: "alpine".into(),
                    name: "x".into(),
                    hostname: None,
                    entrypoint: None,
                    command: Vec::new(),
                    environment: Vec::new(),
                    working_directory: None,
                    user: None,
                    labels: Vec::new(),
                    mounts: Vec::new(),
                    network: None,
                    ports: Vec::new(),
                    memory_mb: None,
                    cpus: None,
                    pids_limit: None,
                },
            },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerStart { id: "c".repeat(64) },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerStop { id: "c".repeat(64) },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerRemove { id: "c".repeat(64) },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerPause { id: "c".repeat(64) },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerUnpause { id: "c".repeat(64) },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerRestart { id: "c".repeat(64) },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerRename {
                id: "c".repeat(64),
                name: "worker-2".into(),
            },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerKill {
                id: "c".repeat(64),
                signal: "SIGTERM".into(),
            },
            Capability::ContainerControl,
        ),
        (
            Request::ExecutionKill {
                id: "e".repeat(32),
                signal: "SIGTERM".into(),
            },
            Capability::ContainerControl,
        ),
        (
            Request::ExecutionRemove { id: "e".repeat(32) },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerExec {
                id: "c".repeat(64),
                command: vec!["worker".into()],
                user: None,
                working_directory: None,
            },
            Capability::ContainerControl,
        ),
        (Request::ImageList, Capability::ImageRead),
        (
            Request::ImagePull {
                reference: "alpine".into(),
            },
            Capability::ImageWrite,
        ),
        (
            Request::ImageInspect {
                reference: "alpine".into(),
            },
            Capability::ImageRead,
        ),
        (
            Request::ImageRemove {
                reference: format!("sha256:{}", "a".repeat(64)),
            },
            Capability::ImageWrite,
        ),
        (Request::ImagePrune, Capability::ImageWrite),
        (Request::TerminalTabs, Capability::TerminalRead),
        (Request::TerminalTopology, Capability::TerminalRead),
        (Request::PaneList, Capability::PaneObserve),
        (
            Request::TerminalOpenTab { title: "logs".into() },
            Capability::TerminalControl,
        ),
        (
            Request::TerminalSplit {
                slot: "s1".into(),
                division: Division::Beside,
            },
            Capability::TerminalControl,
        ),
        (
            Request::TerminalSpawn {
                slot: "s1".into(),
                command: vec!["ls".into()],
            },
            Capability::TerminalControl,
        ),
        (
            Request::TerminalWritePane {
                slot: "s1".into(),
                generation: 1,
                revision: 2,
                contents: b"pwd\n".to_vec(),
            },
            Capability::TerminalControl,
        ),
        (
            Request::TerminalResizeGrid {
                slot: "s1".into(),
                columns: 120,
                rows: 40,
            },
            Capability::TerminalControl,
        ),
        (
            Request::TerminalRetitlePane {
                slot: "s1".into(),
                title: "Build 🧪".into(),
            },
            Capability::TerminalControl,
        ),
        (
            Request::TerminalSwitchOccupant {
                slot: "s1".into(),
                generation: 7,
                target: hl_extension::port::PaneOccupantTarget::Surface {
                    extension: "demo".into(),
                    provider: "main".into(),
                },
            },
            Capability::TerminalControl,
        ),
        (
            Request::FilesystemList { path: path("logs") },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemRead {
                path: path("logs/app.log"),
            },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemStat {
                path: path("logs/app.log"),
            },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemWrite {
                path: path("logs/app.log"),
                contents: b"x".to_vec(),
            },
            Capability::FilesystemWrite,
        ),
        (
            Request::FilesystemMkdir { path: path("logs/new") },
            Capability::FilesystemWrite,
        ),
        (
            Request::FilesystemRename {
                from: path("logs/a"),
                to: path("logs/b"),
            },
            Capability::FilesystemWrite,
        ),
        (
            Request::FilesystemRemove { path: path("logs/old") },
            Capability::FilesystemWrite,
        ),
        (
            Request::InterfaceOpenTab {
                title: "Postgres".into(),
            },
            Capability::Interface,
        ),
    ]
}

#[test]
fn every_call_succeeds_with_its_capability_and_fails_without_it() {
    for (request, capability) in calls() {
        let host = Host::new();

        let mut granted = session(&[capability], &["logs"]);
        assert!(
            granted.dispatch(&request, &services(&host)).is_ok(),
            "{request:?} must be permitted by {capability:?}"
        );

        let refused_host = Host::new();
        let others: Vec<Capability> = Capability::ALL
            .iter()
            .copied()
            .filter(|held| *held != capability)
            .collect();
        let mut refused = session(&others, &["logs"]);
        let failure = refused
            .dispatch(&request, &services(&refused_host))
            .expect_err("must be refused");

        assert!(
            matches!(failure, Failure::Denied { .. }),
            "{request:?} must be refused without {capability:?}, got {failure:?}"
        );
        assert!(
            refused_host.ledger.reached().is_empty(),
            "{request:?} reached {:?} despite being refused",
            refused_host.ledger.reached()
        );
    }
}

#[test]
fn workspace_mutations_require_a_complete_generation_before_host_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::WorkspaceControl], &[]);
    for request in [
        Request::WorkspaceUpdate {
            name: "other".into(),
            generation: "short".into(),
            configuration: workspace_configuration(),
        },
        Request::WorkspaceDelete {
            name: "other".into(),
            generation: String::new(),
        },
    ] {
        assert!(session.dispatch(&request, &services(&host)).is_err());
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn extension_acquisition_identifiers_are_bounded_before_the_host() {
    let host = Host::new();
    let mut session = session(&[Capability::ExtensionInstall], &[]);
    assert!(session
        .dispatch(
            &Request::ExtensionAcquisitionStart {
                reference: "x".repeat(513)
            },
            &services(&host)
        )
        .is_err());
    assert!(session
        .dispatch(
            &Request::ExtensionAcquisitionStart {
                reference: "bad reference".into()
            },
            &services(&host)
        )
        .is_err());
    assert!(session
        .dispatch(
            &Request::ExtensionAcquisitionStatus { job: "x".repeat(129) },
            &services(&host)
        )
        .is_err());
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn extension_acquisition_cancellation_preserves_the_observed_revision() {
    let host = Host::new();
    let mut session = session(&[Capability::ExtensionInstall], &[]);
    assert_eq!(
        session
            .dispatch(
                &Request::ExtensionAcquisitionCancel {
                    job: "job-1".into(),
                    revision: 41,
                },
                &services(&host),
            )
            .unwrap(),
        Reply::Done
    );
    assert_eq!(host.cancelled_revision.get(), Some(41));
}

#[test]
fn extension_controls_refuse_partial_digests_before_host_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::ExtensionControl], &[]);
    for request in [
        Request::ExtensionEnable {
            name: "sample".into(),
            image_digest: "sha256:abc".into(),
        },
        Request::ExtensionDisable {
            name: "sample".into(),
            image_digest: String::new(),
        },
        Request::ExtensionRemove {
            name: "sample".into(),
            image_digest: "sha256:abc".into(),
        },
    ] {
        assert!(session.dispatch(&request, &services(&host)).is_err());
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn terminal_input_and_grid_are_bounded_before_the_window_is_reached() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalControl], &[]);
    let oversized = Request::TerminalWritePane {
        slot: "s1".into(),
        generation: 1,
        revision: 2,
        contents: vec![0; hl_extension::port::PANE_INPUT_BYTES + 1],
    };
    assert!(matches!(
        session.dispatch(&oversized, &services(&host)),
        Err(Failure::Conflict { .. })
    ));
    assert!(host.ledger.reached().is_empty());

    let invalid = Request::TerminalResizeGrid {
        slot: "s1".into(),
        columns: 0,
        rows: 24,
    };
    assert!(matches!(
        session.dispatch(&invalid, &services(&host)),
        Err(Failure::Conflict { .. })
    ));
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn pane_titles_are_utf8_bounded_and_refused_before_terminal_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalControl], &[]);
    for title in [
        String::new(),
        "   ".into(),
        "line\nbreak".into(),
        "nul\0byte".into(),
        "🧪".repeat(65),
    ] {
        assert!(matches!(
            session.dispatch(
                &Request::TerminalRetitlePane {
                    slot: "s1".into(),
                    title
                },
                &services(&host)
            ),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
    assert_eq!(
        session.dispatch(
            &Request::TerminalRetitlePane {
                slot: "s1".into(),
                title: " Build 🧪 ".into()
            },
            &services(&host),
        ),
        Ok(Reply::Done)
    );
    assert_eq!(host.ledger.reached(), ["terminal.retitle"]);
}

#[test]
fn occupant_targets_are_native_names_and_reach_terminal_authority_exactly_once() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalControl], &[]);
    for extension in ["", "Upper", "x/escape", &"x".repeat(65)] {
        let request = Request::TerminalSwitchOccupant {
            slot: "s1".into(),
            generation: 7,
            target: hl_extension::port::PaneOccupantTarget::Surface {
                extension: extension.into(),
                provider: "main".into(),
            },
        };
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
    let request = Request::TerminalSwitchOccupant {
        slot: "s1".into(),
        generation: 7,
        target: hl_extension::port::PaneOccupantTarget::Surface {
            extension: "demo".into(),
            provider: "main".into(),
        },
    };
    assert_eq!(session.dispatch(&request, &services(&host)), Ok(Reply::Done));
    assert_eq!(host.ledger.reached(), ["terminal.switch_occupant"]);
}

#[test]
fn terminal_spawn_argv_is_bounded_before_the_window_is_reached() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalControl], &[]);
    for command in [
        Vec::new(),
        vec![String::new()],
        vec!["x".repeat(hl_extension::port::TERMINAL_COMMAND_ARGUMENT_BYTES + 1)],
        vec!["ok".into(), "contains\0nul".into()],
        vec!["x".repeat(513); hl_extension::port::TERMINAL_COMMAND_ARGUMENTS],
        vec!["x".into(); hl_extension::port::TERMINAL_COMMAND_ARGUMENTS + 1],
    ] {
        assert!(matches!(
            session.dispatch(
                &Request::TerminalSpawn {
                    slot: "s1".into(),
                    command
                },
                &services(&host)
            ),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
    assert_eq!(
        session.dispatch(
            &Request::TerminalSpawn {
                slot: "s1".into(),
                command: vec!["printf".into(), "%s\\n".into(), "ready".into()],
            },
            &services(&host),
        ),
        Ok(Reply::Done)
    );
    assert_eq!(host.ledger.reached(), ["terminal.spawn"]);
}

#[test]
fn configured_container_creation_is_bounded_before_control_authority() {
    use hl_extension::port::{ContainerCreateSpec, ContainerPort, ContainerVolumeMount};
    let host = Host::new();
    let mut authorized = session(
        &[
            Capability::ContainerControl,
            Capability::VolumeRead,
            Capability::NetworkWrite,
        ],
        &[],
    );
    let spec = ContainerCreateSpec {
        image: "alpine:3.20".into(),
        name: "worker".into(),
        hostname: Some("h".repeat(253)),
        entrypoint: Some(vec!["/init".into()]),
        command: vec!["serve".into()],
        environment: vec![("MODE".into(), "agent".into())],
        working_directory: Some("/work".into()),
        user: Some("1000".into()),
        labels: vec![("owner".into(), "agent".into())],
        mounts: vec![ContainerVolumeMount {
            volume: "cache".into(),
            target: "/cache".into(),
            read_only: true,
        }],
        network: Some("private".into()),
        ports: vec![ContainerPort {
            container: 8080,
            host: Some(18080),
            protocol: "tcp".into(),
        }],
        memory_mb: Some(512),
        cpus: Some(2),
        pids_limit: Some(128),
    };
    assert_eq!(
        authorized.dispatch(&Request::ContainerCreate { spec: spec.clone() }, &services(&host)),
        Ok(Reply::Identity("id-worker".into()))
    );
    assert_eq!(host.ledger.reached(), ["containers.create_spec"]);

    let mut invalid_container_name = spec.clone();
    invalid_container_name.name = "-worker".into();
    assert!(matches!(
        authorized.dispatch(
            &Request::ContainerCreate {
                spec: invalid_container_name
            },
            &services(&host)
        ),
        Err(Failure::Conflict { .. })
    ));
    let mut invalid_network = spec.clone();
    invalid_network.network = Some("-private".into());
    assert!(matches!(
        authorized.dispatch(&Request::ContainerCreate { spec: invalid_network }, &services(&host)),
        Err(Failure::Conflict { .. })
    ));
    let mut oversized_network = spec.clone();
    oversized_network.network = Some("n".repeat(256));
    assert!(matches!(
        authorized.dispatch(
            &Request::ContainerCreate {
                spec: oversized_network
            },
            &services(&host)
        ),
        Err(Failure::Conflict { .. })
    ));
    assert_eq!(host.ledger.reached(), ["containers.create_spec"]);

    let mut oversized_hostname = spec.clone();
    oversized_hostname.hostname = Some("h".repeat(254));
    assert!(matches!(
        authorized.dispatch(
            &Request::ContainerCreate {
                spec: oversized_hostname
            },
            &services(&host)
        ),
        Err(Failure::Conflict { .. })
    ));
    let mut injected_hostname = spec.clone();
    injected_hostname.hostname = Some("bad\nname".into());
    assert!(matches!(
        authorized.dispatch(
            &Request::ContainerCreate {
                spec: injected_hostname
            },
            &services(&host)
        ),
        Err(Failure::Conflict { .. })
    ));
    assert_eq!(host.ledger.reached(), ["containers.create_spec"]);

    let mut boundary = spec.clone();
    boundary.environment = vec![
        ("é".repeat(128), "value".into()),
        ("release-name".into(), "value".into()),
    ];
    boundary.mounts[0].volume = "v".repeat(255);
    assert_eq!(
        authorized.dispatch(&Request::ContainerCreate { spec: boundary.clone() }, &services(&host)),
        Ok(Reply::Identity("id-worker".into()))
    );

    let mut oversized_environment_name = boundary;
    oversized_environment_name.environment[0].0.push('é');
    assert!(matches!(
        authorized.dispatch(
            &Request::ContainerCreate {
                spec: oversized_environment_name
            },
            &services(&host)
        ),
        Err(Failure::Conflict { .. })
    ));
    let mut invalid_name = spec.clone();
    invalid_name.environment = vec![("BAD=NAME".into(), "value".into())];
    assert!(matches!(
        authorized.dispatch(&Request::ContainerCreate { spec: invalid_name }, &services(&host)),
        Err(Failure::Conflict { .. })
    ));
    let mut oversized_volume = spec.clone();
    oversized_volume.mounts[0].volume = "v".repeat(256);
    assert!(matches!(
        authorized.dispatch(&Request::ContainerCreate { spec: oversized_volume }, &services(&host)),
        Err(Failure::Conflict { .. })
    ));

    let mut insufficient = session(&[Capability::ContainerControl], &[]);
    assert!(matches!(
        insufficient.dispatch(&Request::ContainerCreate { spec: spec.clone() }, &services(&host)),
        Err(Failure::Denied { .. })
    ));

    let mut escaped = spec;
    escaped.mounts[0].target = "/work/../host".into();
    assert!(matches!(
        authorized.dispatch(&Request::ContainerCreate { spec: escaped }, &services(&host)),
        Err(Failure::Conflict { .. })
    ));
    assert_eq!(
        host.ledger.reached(),
        ["containers.create_spec", "containers.create_spec"],
        "invalid mounts never reach control"
    );
}

#[test]
fn execution_signals_are_bounded_before_the_container_port_is_reached() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerControl], &[]);
    for signal in [String::new(), "x".repeat(33)] {
        assert!(matches!(
            session.dispatch(
                &Request::ExecutionKill {
                    id: "e1".into(),
                    signal,
                },
                &services(&host),
            ),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn execution_removal_refuses_aliases_before_control_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerControl], &[]);
    for id in ["friendly".to_owned(), "e1".to_owned(), "a".repeat(12)] {
        assert!(matches!(
            session.dispatch(&Request::ExecutionRemove { id }, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());

    let id = "e".repeat(32);
    assert_eq!(
        session.dispatch(&Request::ExecutionRemove { id }, &services(&host)),
        Ok(Reply::Done)
    );
    assert_eq!(host.ledger.reached(), ["executions.remove"]);
}

#[test]
fn lifecycle_controls_refuse_snapshot_pids_names_and_prefixes_before_control_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerControl], &[]);
    for request in [
        Request::ContainerStart { id: "friendly-name".into() },
        Request::ContainerPause { id: "a".repeat(12) },
        Request::ContainerUnpause { id: "friendly-name".into() },
        Request::ContainerRestart { id: "1".into() },
        Request::ContainerStop {
            id: "friendly-name".into(),
        },
        Request::ContainerRemove { id: "a".repeat(12) },
        Request::ContainerKill {
            id: "1".into(),
            signal: "SIGTERM".into(),
        },
        Request::ContainerKill {
            id: "friendly-name".into(),
            signal: "SIGTERM".into(),
        },
        Request::ContainerKill {
            id: "a".repeat(12),
            signal: "SIGTERM".into(),
        },
        Request::ExecutionKill {
            id: "7".into(),
            signal: "SIGTERM".into(),
        },
        Request::ExecutionKill {
            id: "b".repeat(12),
            signal: "SIGTERM".into(),
        },
    ] {
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());

    session
        .dispatch(&Request::ContainerStop { id: "a".repeat(64) }, &services(&host))
        .unwrap();
    session
        .dispatch(&Request::ContainerRemove { id: "a".repeat(64) }, &services(&host))
        .unwrap();
    session
        .dispatch(
            &Request::ContainerKill {
                id: "a".repeat(64),
                signal: "SIGTERM".into(),
            },
            &services(&host),
        )
        .unwrap();
    session
        .dispatch(
            &Request::ExecutionKill {
                id: "b".repeat(32),
                signal: "SIGTERM".into(),
            },
            &services(&host),
        )
        .unwrap();
    assert_eq!(
        host.ledger.reached(),
        [
            "containers.stop",
            "containers.remove",
            "containers.kill",
            "executions.kill"
        ]
    );
}

#[test]
fn container_rename_requires_immutable_identity_and_native_name_grammar() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerControl], &[]);
    for request in [
        Request::ContainerRename { id: "friendly-name".into(), name: "worker".into() },
        Request::ContainerRename { id: "a".repeat(12), name: "worker".into() },
        Request::ContainerRename { id: "a".repeat(64), name: ".worker".into() },
        Request::ContainerRename { id: "a".repeat(64), name: "worker/name".into() },
        Request::ContainerRename { id: "a".repeat(64), name: "x".repeat(129) },
    ] {
        assert!(matches!(session.dispatch(&request, &services(&host)), Err(Failure::Conflict { .. })));
    }
    assert!(host.ledger.reached().is_empty());
    session.dispatch(
        &Request::ContainerRename { id: "a".repeat(64), name: "worker_2.prod".into() },
        &services(&host),
    ).unwrap();
    assert_eq!(host.ledger.reached(), ["containers.rename"]);
}

#[test]
fn image_removal_refuses_mutable_tags_and_partial_digests_before_control_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::ImageWrite], &[]);
    for reference in ["alpine:latest".to_owned(), "sha256:abc".to_owned(), "a".repeat(64)] {
        assert!(matches!(
            session.dispatch(&Request::ImageRemove { reference }, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
    session
        .dispatch(
            &Request::ImageRemove {
                reference: format!("sha256:{}", "a".repeat(64)),
            },
            &services(&host),
        )
        .unwrap();
    assert_eq!(host.ledger.reached(), ["images.remove"]);
}

#[test]
fn network_mutations_refuse_names_prefixes_and_container_aliases_before_control_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::NetworkWrite], &[]);
    for request in [
        Request::NetworkRemove {
            reference: "private".into(),
        },
        Request::NetworkConnect {
            reference: "a".repeat(12),
            container: "b".repeat(64),
            aliases: Vec::new(),
        },
        Request::NetworkDisconnect {
            reference: "a".repeat(32),
            container: "friendly".into(),
        },
    ] {
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
    session
        .dispatch(
            &Request::NetworkRemove {
                reference: "a".repeat(32),
            },
            &services(&host),
        )
        .unwrap();
    session
        .dispatch(
            &Request::NetworkConnect {
                reference: "a".repeat(32),
                container: "b".repeat(64),
                aliases: Vec::new(),
            },
            &services(&host),
        )
        .unwrap();
    session
        .dispatch(
            &Request::NetworkDisconnect {
                reference: "a".repeat(32),
                container: "b".repeat(64),
            },
            &services(&host),
        )
        .unwrap();
    assert_eq!(
        host.ledger.reached(),
        ["networks.remove", "networks.connect", "networks.disconnect"]
    );
}

#[test]
fn network_endpoint_alias_boundaries_are_enforced_before_control_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::NetworkWrite], &[]);
    let request = |aliases| Request::NetworkConnect {
        reference: "a".repeat(32),
        container: "b".repeat(64),
        aliases,
    };
    for aliases in [
        vec!["same".into(), "same".into()],
        vec!["-leading".into()],
        vec!["é".into()],
        vec!["x".repeat(254)],
        (0..65).map(|index| format!("alias-{index}")).collect(),
    ] {
        assert!(matches!(
            session.dispatch(&request(aliases), &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
    let mut aliases = (0..64).map(|index| format!("alias-{index}")).collect::<Vec<_>>();
    aliases[0] = "x".repeat(253);
    session.dispatch(&request(aliases), &services(&host)).unwrap();
    assert_eq!(host.ledger.reached(), ["networks.connect"]);
}

#[test]
fn volume_removal_requires_the_exact_observed_generation() {
    let host = Host::new();
    let mut session = session(&[Capability::VolumeWrite], &[]);
    assert!(matches!(
        session.dispatch(
            &Request::VolumeRemove { name: "cache".into(), generation: "legacy-or-stale".into() },
            &services(&host),
        ),
        Err(Failure::Conflict { .. })
    ));
    assert!(host.ledger.reached().is_empty());
    session.dispatch(
        &Request::VolumeRemove { name: "cache".into(), generation: "a".repeat(32) },
        &services(&host),
    ).unwrap();
    assert_eq!(host.ledger.reached(), ["volumes.remove"]);
}

#[test]
fn a_refusal_is_reported_rather_than_answered_emptily() {
    let host = Host::new();
    let mut session = session(&[Capability::WorkspaceRead], &[]);

    let failure = session
        .dispatch(&Request::ContainerList, &services(&host))
        .expect_err("refused");

    match failure {
        Failure::Denied { capability, .. } => assert_eq!(capability, "container-read"),
        other => panic!("an empty list would be worse than a refusal, got {other:?}"),
    }
}

#[test]
fn a_path_outside_the_declared_roots_is_refused_before_the_service() {
    let host = Host::new();
    let mut session = session(&[Capability::FilesystemRead], &["logs"]);

    let failure = session
        .dispatch(
            &Request::FilesystemRead {
                path: path("state/secret"),
            },
            &services(&host),
        )
        .expect_err("refused");

    assert!(matches!(failure, Failure::Denied { .. }));
    assert!(host.ledger.reached().is_empty(), "confinement precedes the read");
}

#[test]
fn a_rename_destination_outside_the_declared_roots_is_refused_before_the_service() {
    let host = Host::new();
    let mut session = session(&[Capability::FilesystemWrite], &["logs"]);
    let failure = session
        .dispatch(
            &Request::FilesystemRename {
                from: path("logs/old"),
                to: path("state/new"),
            },
            &services(&host),
        )
        .expect_err("destination refused");
    assert!(matches!(failure, Failure::Denied { .. }));
    assert!(
        host.ledger.reached().is_empty(),
        "both paths are confined before rename"
    );
}

#[test]
fn holding_read_never_permits_the_matching_write() {
    let host = Host::new();
    let mut session = session(&[Capability::FilesystemRead, Capability::ContainerRead], &["logs"]);

    assert!(session
        .dispatch(
            &Request::FilesystemWrite {
                path: path("logs/app.log"),
                contents: b"x".to_vec()
            },
            &services(&host)
        )
        .is_err());
    assert!(session
        .dispatch(&Request::ContainerStop { id: "c1".into() }, &services(&host))
        .is_err());
    assert!(session
        .dispatch(
            &Request::ContainerKill {
                id: "c1".into(),
                signal: "SIGKILL".into(),
            },
            &services(&host),
        )
        .is_err());
    assert!(session
        .dispatch(
            &Request::ContainerExec {
                id: "c1".into(),
                command: vec!["sh".into()],
                user: None,
                working_directory: None,
            },
            &services(&host),
        )
        .is_err());
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn deep_container_reads_return_typed_processes_logs_and_execution_state() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);

    let processes = session
        .dispatch(&Request::ContainerProcesses { id: "c1".into() }, &services(&host))
        .expect("process table");
    assert!(matches!(processes, Reply::Processes(table)
        if table.titles == ["PID", "CMD"] && table.observed_at_ms == 1_700_000_000_000
            && table.scope == hl_extension::port::ProcessScope::Initial
            && table.pid_identity == hl_extension::port::ProcessPidIdentity::Snapshot
            && !table.truncated));

    let logs = session
        .dispatch(
            &Request::ContainerLogs {
                id: "c1".into(),
                stdout: true,
                stderr: false,
            },
            &services(&host),
        )
        .expect("logs");
    assert!(matches!(logs, Reply::Logs(output)
        if output.stdout == b"ready\n" && !output.truncated && !output.eof
            && !output.stdout_truncated && !output.stderr_truncated));

    let execution = session
        .dispatch(&Request::ExecutionInspect { id: "e".repeat(32) }, &services(&host))
        .expect("execution");
    assert!(matches!(execution, Reply::Execution(execution) if execution.id == "e".repeat(32) && execution.running));

    let output = session
        .dispatch(
            &Request::ExecutionLogs {
                id: "e".repeat(32),
                stdout: true,
                stderr: true,
            },
            &services(&host),
        )
        .expect("execution output");
    assert!(matches!(output, Reply::Logs(output) if output.eof && !output.truncated));

    let waited = session
        .dispatch(
            &Request::ExecutionWait {
                id: "e".repeat(32),
                timeout_ms: 500,
            },
            &services(&host),
        )
        .expect("execution wait");
    assert!(matches!(waited, Reply::Execution(execution) if !execution.running && execution.exit_code == 17));
}

#[test]
fn execution_wait_rejects_unbounded_timeout_before_calling_host() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    assert!(session
        .dispatch(
            &Request::ExecutionWait {
                id: "e".repeat(32),
                timeout_ms: 30_001
            },
            &services(&host)
        )
        .is_err());
    assert!(!host.ledger.reached().contains(&"executions.wait"));
}

#[test]
fn execution_reads_refuse_names_and_prefixes_before_inventory_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    for request in [
        Request::ExecutionInspect { id: "worker".into() },
        Request::ExecutionLogs { id: "a".repeat(12), stdout: true, stderr: false },
        Request::ExecutionWait { id: "7".into(), timeout_ms: 500 },
    ] {
        assert!(matches!(session.dispatch(&request, &services(&host)), Err(Failure::Conflict { .. })));
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn execution_logs_require_a_stream_before_calling_host() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    assert!(session
        .dispatch(
            &Request::ExecutionLogs {
                id: "e".repeat(32),
                stdout: false,
                stderr: false
            },
            &services(&host)
        )
        .is_err());
    assert!(!host.ledger.reached().contains(&"executions.logs"));
}

#[test]
fn container_exec_returns_the_real_execution_identity() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerControl], &[]);
    let immutable = "c".repeat(64);
    let refused = session.dispatch(
        &Request::ContainerExec {
            id: "worker".into(),
            command: vec!["worker".into()],
            user: None,
            working_directory: None,
        },
        &services(&host),
    );
    assert!(matches!(refused, Err(Failure::Conflict { .. })));
    assert!(host.ledger.reached().is_empty(), "a mutable alias reached execution authority");
    let reply = session
        .dispatch(
            &Request::ContainerExec {
                id: immutable,
                command: vec!["worker".into()],
                user: Some("1000".into()),
                working_directory: Some("/work".into()),
            },
            &services(&host),
        )
        .expect("exec starts");
    assert_eq!(reply, Reply::Identity("e1".into()));
    assert_eq!(host.ledger.reached(), vec!["containers.exec"]);
}

#[test]
fn volume_and_network_reads_and_safe_controls_use_distinct_grants() {
    let host = Host::new();
    let mut read = session(&[Capability::VolumeRead, Capability::NetworkRead], &[]);
    assert!(
        matches!(read.dispatch(&Request::VolumeList, &services(&host)), Ok(Reply::Volumes(values)) if values[0].name == "cache")
    );
    assert!(
        matches!(read.dispatch(&Request::NetworkInspect { reference: "private".into() }, &services(&host)), Ok(Reply::Network(value)) if value.id == "a".repeat(32))
    );
    assert!(matches!(
        read.dispatch(&Request::VolumeCreate { name: "unsafe".into() }, &services(&host)),
        Err(Failure::Denied { .. })
    ));

    let mut write = session(&[Capability::VolumeWrite, Capability::NetworkWrite], &[]);
    assert!(
        matches!(write.dispatch(&Request::VolumeCreate { name: "cache".into() }, &services(&host)), Ok(Reply::Volume(value)) if value.name == "cache")
    );
    assert_eq!(
        write.dispatch(&Request::NetworkCreate { name: "private".into() }, &services(&host)),
        Ok(Reply::Identity("a".repeat(32)))
    );
    assert_eq!(
        write.dispatch(
            &Request::NetworkConnect {
                reference: "a".repeat(32),
                container: "b".repeat(64),
                aliases: Vec::new(),
            },
            &services(&host)
        ),
        Ok(Reply::Done)
    );
    assert_eq!(
        write.dispatch(
            &Request::NetworkDisconnect {
                reference: "a".repeat(32),
                container: "b".repeat(64)
            },
            &services(&host)
        ),
        Ok(Reply::Done)
    );
    assert!(matches!(
        write.dispatch(&Request::NetworkList, &services(&host)),
        Err(Failure::Denied { .. })
    ));
}

#[test]
fn a_host_failure_is_distinguished_from_a_refusal() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);

    let failure = session
        .dispatch(&Request::ContainerInspect { id: "missing".into() }, &services(&host))
        .expect_err("absent");

    assert!(
        matches!(failure, Failure::Absent { .. }),
        "'it does not exist' must not read as 'you may not', got {failure:?}"
    );
    assert_eq!(host.ledger.reached(), vec!["containers.inspect"]);
}

#[test]
fn revoking_a_capability_stops_an_established_subscription() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);

    session
        .dispatch(
            &Request::EventSubscribe {
                topic: Topic::Containers,
            },
            &services(&host),
        )
        .expect("subscribed");
    assert!(session.may_emit(Topic::Containers));

    session.authority_mut().revoke(Capability::ContainerRead);

    assert!(
        !session.may_emit(Topic::Containers),
        "a grant is re-checked at emission, not only at subscribe"
    );
}

#[test]
fn a_topic_cannot_be_followed_without_its_namespace_capability() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);

    assert!(session
        .dispatch(&Request::EventSubscribe { topic: Topic::Terminal }, &services(&host))
        .is_err());
    assert!(!session.may_emit(Topic::Terminal));
}

#[test]
fn unsubscribing_stops_emission() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    let services = services(&host);

    session
        .dispatch(
            &Request::EventSubscribe {
                topic: Topic::Containers,
            },
            &services,
        )
        .expect("subscribed");
    session
        .dispatch(
            &Request::EventUnsubscribe {
                topic: Topic::Containers,
            },
            &services,
        )
        .expect("unsubscribed");

    assert!(!session.may_emit(Topic::Containers));
    assert!(session.topics().is_empty());
}

#[test]
fn a_session_records_each_surface_it_opens() {
    let host = Host::new();
    let mut session = session(&[Capability::Interface], &[]);
    let services = services(&host);
    let first = session
        .dispatch(
            &Request::InterfaceOpenTab {
                title: "Postgres".into(),
            },
            &services,
        )
        .expect("opened");
    let second = session
        .dispatch(&Request::InterfaceOpenTab { title: "Logs".into() }, &services)
        .expect("opened again");

    assert_ne!(first, second);
    assert_eq!(
        host.ledger.reached(),
        vec!["terminal.open_tab", "terminal.open_tab"],
        "each independent tree gets a real host surface"
    );
    assert_eq!(
        session.tab(),
        None,
        "there is no truthful singular identity after two opens"
    );
}

#[test]
fn addressed_frames_remain_separate_across_two_owned_surfaces() {
    let host = Host::new();
    let mut session = session(&[Capability::Interface], &[]);
    let services = services(&host);
    for title in ["Containers", "Logs"] {
        session
            .dispatch(&Request::InterfaceOpenTab { title: title.into() }, &services)
            .expect("surface opened");
    }
    let first = hl_gui::Frame::new(7);
    let second = hl_gui::Frame::new(3);
    session
        .dispatch(
            &Request::InterfaceRenderAt {
                slot: "tab-Containers".into(),
                frame: first.clone(),
            },
            &services,
        )
        .expect("first surface rendered");
    session
        .dispatch(
            &Request::InterfaceRenderAt {
                slot: "tab-Logs".into(),
                frame: second.clone(),
            },
            &services,
        )
        .expect("second surface rendered");

    let drained = session.drain();
    assert_eq!(drained[0].slot, "tab-Containers");
    assert_eq!(drained[0].frame, first);
    assert_eq!(drained[1].slot, "tab-Logs");
    assert_eq!(drained[1].frame, second);
    let mutation = hl_gui::SourceMutation::Length {
        source: hl_gui::SourceId::new(4),
        version: hl_gui::Version::new(2),
        rows: 100_000,
    };
    session
        .dispatch(
            &Request::SourceResizeAt {
                slot: "tab-Logs".into(),
                mutation: mutation.clone(),
            },
            &services,
        )
        .expect("second surface source resized");
    let mutations = session.drain_sources();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0].slot, "tab-Logs");
    assert_eq!(mutations[0].mutation, mutation);
    assert!(
        session
            .dispatch(
                &Request::InterfaceRender {
                    frame: hl_gui::Frame::new(8),
                },
                &services,
            )
            .is_err(),
        "legacy unaddressed rendering cannot silently choose between surfaces"
    );
    assert!(
        session
            .dispatch(
                &Request::InterfaceRenderAt {
                    slot: "somebody-elses-pane".into(),
                    frame: hl_gui::Frame::new(9),
                },
                &services,
            )
            .is_err(),
        "addressing does not grant authority over arbitrary workspace panes"
    );
}

#[test]
fn withdrawing_one_owned_surface_preserves_its_sibling() {
    let host = Host::new();
    let mut session = session(&[Capability::Interface], &[]);
    let services = services(&host);
    for title in ["Containers", "Logs"] {
        session
            .dispatch(&Request::InterfaceOpenTab { title: title.into() }, &services)
            .expect("surface opened");
    }
    session
        .dispatch(
            &Request::InterfaceWithdraw {
                slot: "tab-Containers".into(),
            },
            &services,
        )
        .expect("owned surface withdrawn");
    assert!(matches!(
        session.dispatch(
            &Request::InterfaceRenderAt {
                slot: "tab-Containers".into(),
                frame: hl_gui::Frame::new(1),
            },
            &services,
        ),
        Err(Failure::Conflict { .. })
    ));
    session
        .dispatch(
            &Request::InterfaceRenderAt {
                slot: "tab-Logs".into(),
                frame: hl_gui::Frame::new(2),
            },
            &services,
        )
        .expect("sibling remains owned");
    assert_eq!(session.drain()[0].slot, "tab-Logs");
    assert!(host.ledger.reached().contains(&"terminal.close"));
    assert_eq!(
        Request::InterfaceWithdraw { slot: "x".into() }.capability(),
        Capability::Interface
    );
}

#[test]
fn a_session_cannot_accumulate_unbounded_interface_surfaces() {
    let host = Host::new();
    let mut session = session(&[Capability::Interface], &[]);
    let services = services(&host);
    for index in 0..32 {
        session
            .dispatch(
                &Request::InterfaceOpenTab {
                    title: format!("surface-{index}"),
                },
                &services,
            )
            .expect("within the surface bound");
    }
    let failure = session
        .dispatch(
            &Request::InterfaceOpenTab {
                title: "overflow".into(),
            },
            &services,
        )
        .expect_err("surface registry is bounded");
    assert!(matches!(failure, Failure::Conflict { .. }));
    assert_eq!(
        host.ledger.reached().len(),
        32,
        "the refused surface never reaches the window adapter"
    );
}

#[test]
fn a_granted_call_reaches_exactly_one_service() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);

    let reply = session
        .dispatch(&Request::ContainerList, &services(&host))
        .expect("permitted");

    assert!(matches!(reply, Reply::Containers(containers) if containers.len() == 1));
    assert_eq!(host.ledger.reached(), vec!["containers.list"]);
}

#[test]
fn container_attachment_requires_its_dedicated_grant_and_preserves_exact_argv() {
    let request = Request::ContainerAttachTerminal {
        id: "a".repeat(64),
        command: vec!["sh".into(), "-lc".into(), "printf '%s' \"$HOME\"".into()],
    };
    let host = Host::new();
    let mut denied = session(&[Capability::ContainerControl, Capability::TerminalControl], &[]);
    assert!(matches!(
        denied.dispatch(&request, &services(&host)),
        Err(Failure::Denied { .. })
    ));
    assert!(host.ledger.reached().is_empty());

    let mut granted = session(&[Capability::ContainerAttach], &[]);
    assert!(matches!(
        granted.dispatch(&request, &services(&host)).expect("dedicated grant"),
        Reply::Identity(ref slot) if slot == "attached-pane"
    ));
    assert_eq!(host.ledger.reached(), vec!["terminal.attach_container"]);

    let invalid = Request::ContainerAttachTerminal {
        id: "friendly".into(),
        command: vec!["sh".into()],
    };
    assert!(matches!(
        granted.dispatch(&invalid, &services(&host)),
        Err(Failure::Conflict { .. })
    ));
}

#[test]
fn an_empty_grant_reaches_nothing_at_all() {
    for (request, _) in calls() {
        let host = Host::new();
        let mut session = session(&[], &["logs"]);

        assert!(
            session.dispatch(&request, &services(&host)).is_err(),
            "{request:?} must be refused with no grant"
        );
        assert!(host.ledger.reached().is_empty());
    }
}

#[test]
fn an_interface_is_rendered_only_into_a_tab_the_session_opened() {
    let host = Host::new();
    let mut session = session(&[Capability::Interface], &[]);
    let services = services(&host);

    let mut surface = hl_gui::Surface::new();
    let card = surface.create(hl_gui::Tag::Card);
    surface.append(hl_gui::NodeId::ROOT, card);
    let frame = surface.frame();

    let premature = session
        .dispatch(&Request::InterfaceRender { frame: frame.clone() }, &services)
        .expect_err("nowhere to draw");
    assert!(
        matches!(premature, Failure::Conflict { .. }),
        "the grant is present and only the order is wrong, got {premature:?}"
    );

    session
        .dispatch(
            &Request::InterfaceOpenTab {
                title: "Containers".into(),
            },
            &services,
        )
        .expect("opened");
    session
        .dispatch(&Request::InterfaceRender { frame: frame.clone() }, &services)
        .expect("rendered");

    let collected = session.drain();
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].slot, "tab-Containers");
    assert_eq!(collected[0].frame, frame, "the host receives exactly what was sent");
    assert!(session.drain().is_empty(), "frames are handed over once");
}

#[test]
fn rendering_an_interface_requires_the_interface_capability() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    let frame = hl_gui::Surface::new().frame();

    let failure = session
        .dispatch(&Request::InterfaceRender { frame }, &services(&host))
        .expect_err("refused");

    assert!(matches!(failure, Failure::Denied { .. }));
    assert!(host.ledger.reached().is_empty());
}
