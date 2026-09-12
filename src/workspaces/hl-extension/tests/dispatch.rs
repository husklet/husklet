//! Dispatch and enforcement, driven entirely by in-memory ports.
//!
//! No container runtime, no socket, no toolkit. That this suite runs at all is
//! the evidence that the ports-and-adapters split is real rather than
//! decorative: if the protocol had reached for a service directly, none of this
//! could be written.

use std::cell::{Cell, RefCell};

use hl_extension::port::{
    ContainerControl, ContainerInventory, ContainerOutput, ContainerSummary, DirectoryPage, Division, Entry,
    ExecutionSummary, ExtensionAcquisitionJob, ExtensionAcquisitionStatus, ExtensionCredential, ExtensionState,
    ExtensionStateStore, ExtensionStore, ExtensionSummary, FileInventory, FileRange, FileRangeRequest, GridSize,
    HostError, ImageDetails, ImagePruneResult, ImageStore, ImageSummary, Occupant, PaneSemanticAction,
    PaneSemanticTree, PaneSummary, PaneText, PreferenceValue, ProcessList, SemanticActionKind, SemanticNode,
    TabSummary, TerminalSurface, TerminalTopology, WorkspaceFiles, WorkspaceInventory, WorkspaceState,
};
use hl_extension::{
    Authority, Capability, ExtensionName, Failure, Grant, RelativePath, Reply, Request, Services, Session, Topic,
    WorkspaceConfiguration, WorkspaceInfo, WorkspaceTerminal,
};

const COMMAND_OWNER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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

    fn clear(&self) {
        self.reached.borrow_mut().clear();
    }
}

struct Host {
    ledger: Ledger,
    execution_input: RefCell<Vec<Vec<u8>>>,
    cancelled_revision: Cell<Option<u64>>,
    fail_notification: Cell<bool>,
    execution_container: RefCell<String>,
}

impl hl_extension::NotificationSink for Host {
    fn publish(&self, _notification: &hl_extension::Notification) -> Result<(), HostError> {
        self.ledger.note("notifications.publish");
        if self.fail_notification.get() {
            Err(HostError::Failed("desktop unavailable".into()))
        } else {
            Ok(())
        }
    }
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
        Ok(vec![
            hl_extension::port::NetworkSummary {
                id: "a".repeat(32),
                name: "private".into(),
                driver: "bridge".into(),
                scope: "local".into(),
                kind: hl_extension::NetworkKind::Custom,
                endpoints: None,
            },
            hl_extension::port::NetworkSummary {
                id: "f".repeat(32),
                name: "unrelated".into(),
                driver: "bridge".into(),
                scope: "local".into(),
                kind: hl_extension::NetworkKind::Custom,
                endpoints: None,
            },
        ])
    }
    fn inspect(&self, reference: &str) -> Result<hl_extension::port::NetworkSummary, HostError> {
        self.ledger.note("networks.inspect");
        Ok(hl_extension::port::NetworkSummary {
            id: "a".repeat(32),
            name: reference.into(),
            driver: "bridge".into(),
            scope: "local".into(),
            kind: hl_extension::NetworkKind::Custom,
            endpoints: Some(hl_extension::port::NetworkEndpointInventory {
                containers: vec!["b".repeat(32)],
                truncated: false,
            }),
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
            execution_input: RefCell::new(Vec::new()),
            cancelled_revision: Cell::new(None),
            fail_notification: Cell::new(false),
            execution_container: RefCell::new("c1".into()),
        }
    }

    fn container() -> ContainerSummary {
        ContainerSummary {
            id: "c1".into(),
            name: "api".into(),
            image: "husklet/api:1".into(),
            state: "running".into(),
            created: 0,
            generation: 0,
            ports: Vec::new(),
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
        if matches!(id, "c1" | "workspace") {
            return Ok(Self::container());
        }
        Err(HostError::Absent(id.into()))
    }

    fn processes(
        &self,
        _id: &str,
        _snapshot: Option<&str>,
        _after: u32,
        _limit: u16,
    ) -> Result<ProcessList, HostError> {
        self.ledger.note("containers.processes");
        Ok(ProcessList {
            container_id: "c".repeat(64),
            titles: vec!["PID".into(), "CMD".into()],
            processes: vec![vec!["7".into(), "server".into()]],
            snapshot: "a".repeat(64),
            next: None,
            more: false,
            observed_at_ms: 1_700_000_000_000,
            scope: hl_extension::port::ProcessScope::Namespace,
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
            container_id: self.execution_container.borrow().clone(),
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
    fn execution_output(
        &self,
        _id: &str,
        after: u64,
        _limit: u16,
    ) -> Result<hl_extension::port::ExecutionOutputPage, HostError> {
        self.ledger.note("executions.output");
        Ok(hl_extension::port::ExecutionOutputPage {
            entries: vec![hl_extension::port::ExecutionOutputEntry {
                sequence: after + 1,
                timestamp_ms: 7,
                stream: "stdout".into(),
                bytes: b"row\n".to_vec(),
            }],
            next: after + 1,
            more: false,
            eof: false,
            gap: false,
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

    fn start(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        self.ledger.note("containers.start");
        Ok(())
    }

    fn stop(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        self.ledger.note("containers.stop");
        Ok(())
    }

    fn remove(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        self.ledger.note("containers.remove");
        Ok(())
    }

    fn pause(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        self.ledger.note("containers.pause");
        Ok(())
    }

    fn unpause(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        self.ledger.note("containers.unpause");
        Ok(())
    }

    fn restart(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        self.ledger.note("containers.restart");
        Ok(())
    }

    fn rename(&self, _id: &str, _expected_id: &str, _generation: u64, _name: &str) -> Result<(), HostError> {
        self.ledger.note("containers.rename");
        Ok(())
    }

    fn kill(&self, _id: &str, _expected_id: &str, _generation: u64, _signal: &str) -> Result<(), HostError> {
        self.ledger.note("containers.kill");
        Ok(())
    }

    fn execution_kill(&self, _id: &str, _signal: &str) -> Result<(), HostError> {
        self.ledger.note("executions.kill");
        Ok(())
    }
    fn execution_cancel(&self, _id: &str, _signal: &str, _timeout_ms: u32) -> Result<(), HostError> {
        self.ledger.note("executions.cancel");
        Ok(())
    }
    fn execution_remove(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("executions.remove");
        Ok(())
    }
    fn execution_write(&self, _id: &str, contents: &[u8]) -> Result<(), HostError> {
        self.ledger.note("executions.write");
        self.execution_input.borrow_mut().push(contents.to_vec());
        Ok(())
    }
    fn execution_close_input(&self, _id: &str) -> Result<(), HostError> {
        self.ledger.note("executions.close_input");
        Ok(())
    }

    fn execute(
        &self,
        _id: &str,
        _expected_id: &str,
        _generation: u64,
        _command: &[String],
        _environment: &[(String, hl_extension::ExecEnvironmentValue)],
        _user: Option<&str>,
        _working_directory: Option<&str>,
        _stdin: bool,
    ) -> Result<String, HostError> {
        self.ledger.note("containers.exec");
        Ok("e".repeat(32))
    }
}

impl ImageStore for Host {
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        self.ledger.note("images.list");
        Ok(Vec::new())
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
    fn attach_container(&self, _id: &str, generation: u64, _command: &[String]) -> Result<String, HostError> {
        assert_eq!(generation, 0, "dispatch forwards the resolved immutable generation");
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
            pinned: false,
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

    fn pin_tab(&self, _tab: &str, _pinned: bool) -> Result<(), HostError> {
        self.ledger.note("terminal.pin_tab");
        Ok(())
    }

    fn focus_tab(&self, _tab: &str) -> Result<(), HostError> {
        self.ledger.note("terminal.focus_tab");
        Ok(())
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
        self.ledger.note("terminal.focus");
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
    assert!(
        session(&[Capability::PaneSemanticRead], &[])
            .dispatch(&action, &services(&host))
            .is_err()
    );
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
    assert!(
        session(&[], &[])
            .dispatch(&Request::PaneList, &services(&host))
            .is_err()
    );
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
fn supervised_terminal_command_has_owned_identity_output_input_and_completion_without_container_grants() {
    let host = Host::new();
    let ownership = hl_extension::ExecutionOwnership::default();
    let mut active = session(
        &[
            Capability::TerminalProcessControl,
            Capability::TerminalOutput,
            Capability::TerminalInput,
        ],
        &[],
    )
    .with_execution_ownership(ownership.clone());
    let started = active
        .dispatch(
            &Request::TerminalCommandStart {
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                command: vec!["sh".into(), "-lc".into(), "printf ready; exit 17".into()],
                working_directory: Some("/work".into()),
                stdin: true,
            },
            &services(&host),
        )
        .expect("an exact pane snapshot may own a supervised command");
    let Reply::TerminalCommand(started) = started else {
        panic!("wrong start reply")
    };
    assert_eq!(started.id, "e".repeat(32));
    assert_eq!(started.owner, COMMAND_OWNER);
    assert_eq!(
        (started.slot.as_str(), started.generation, started.revision),
        ("s1", 0, 0)
    );
    assert_eq!(
        host.ledger.reached(),
        vec!["containers.inspect", "containers.exec", "executions.inspect"]
    );

    host.ledger.clear();
    let written = active
        .dispatch(
            &Request::TerminalCommandWrite {
                id: started.id.clone(),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                contents: b"question\n".to_vec(),
            },
            &services(&host),
        )
        .expect("owned input");
    assert_eq!(
        written,
        Reply::TerminalCommandInput(hl_extension::port::TerminalCommandInput {
            id: started.id.clone(),
            committed: 9,
        })
    );

    let output = active
        .dispatch(
            &Request::TerminalCommandOutput {
                id: started.id.clone(),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                after: 0,
                limit: 4,
            },
            &services(&host),
        )
        .expect("owned output");
    let Reply::TerminalCommandOutput(output) = output else {
        panic!("wrong output reply")
    };
    assert_eq!((output.slot.as_str(), output.generation, output.revision), ("s1", 0, 0));
    assert_eq!(output.output.entries[0].bytes, b"row\n");

    let completed = active
        .dispatch(
            &Request::TerminalCommandWait {
                id: started.id.clone(),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                timeout_ms: 500,
            },
            &services(&host),
        )
        .expect("authoritative completion");
    let Reply::TerminalCommand(completed) = completed else {
        panic!("wrong wait reply")
    };
    assert!(!completed.running);
    assert_eq!(completed.exit_code, 17);

    host.ledger.clear();
    let foreign = Session::new(Authority::new(
        ExtensionName::new("foreign").expect("name"),
        Grant::new([Capability::TerminalOutput]),
        Vec::new(),
    ))
    .with_extension_identity("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    .dispatch(
        &Request::TerminalCommandInspect {
            id: started.id.clone(),
            owner: started.owner.clone(),
            slot: "s1".into(),
            generation: 0,
            revision: 0,
        },
        &services(&host),
    );
    assert!(matches!(foreign, Err(Failure::Denied { .. })));
    assert!(host.ledger.reached().is_empty(), "foreign ownership must be fenced before host lookup");

    let resumed = session(&[Capability::TerminalOutput], &[])
        .with_execution_ownership(ownership.clone())
        .dispatch(
            &Request::TerminalCommandInspect {
                id: started.id,
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
            },
            &services(&host),
        )
        .expect("immutable command identity resumes on a reconnected extension session");
    let Reply::TerminalCommand(resumed) = resumed else {
        panic!("wrong resume reply")
    };
    assert_eq!(
        (resumed.slot.as_str(), resumed.generation, resumed.revision),
        ("s1", 0, 0)
    );

    host.ledger.clear();
    let cancelled = session(&[Capability::TerminalProcessControl], &[])
        .with_execution_ownership(ownership)
        .dispatch(
            &Request::TerminalCommandCancel {
                id: resumed.id,
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                signal: "SIGTERM".into(),
                timeout_ms: 500,
            },
            &services(&host),
        )
        .expect("a reconnected controller can cancel the immutable command");
    assert!(matches!(cancelled, Reply::TerminalCommand(_)));
    assert_eq!(host.ledger.reached(), vec!["executions.cancel", "executions.inspect"]);
}

#[test]
fn terminal_command_identity_cannot_be_forged_around_a_foreign_execution() {
    let host = Host::new();
    let id = "e".repeat(32);
    let mut attacker = session(
        &[
            Capability::TerminalOutput,
            Capability::TerminalInput,
            Capability::TerminalProcessControl,
        ],
        &[],
    );
    let requests = [
        Request::TerminalCommandInspect {
            id: id.clone(),
            owner: COMMAND_OWNER.into(),
            slot: "s1".into(),
            generation: 0,
            revision: 0,
        },
        Request::TerminalCommandOutput {
            id: id.clone(),
            owner: COMMAND_OWNER.into(),
            slot: "s1".into(),
            generation: 0,
            revision: 0,
            after: 0,
            limit: 1,
        },
        Request::TerminalCommandWrite {
            id: id.clone(),
            owner: COMMAND_OWNER.into(),
            slot: "s1".into(),
            generation: 0,
            revision: 0,
            contents: vec![0, 3, b'\n', 255],
        },
        Request::TerminalCommandCloseInput {
            id: id.clone(),
            owner: COMMAND_OWNER.into(),
            slot: "s1".into(),
            generation: 0,
            revision: 0,
        },
        Request::TerminalCommandCancel {
            id,
            owner: COMMAND_OWNER.into(),
            slot: "s1".into(),
            generation: 0,
            revision: 0,
            signal: "SIGTERM".into(),
            timeout_ms: 1,
        },
    ];
    for request in requests {
        assert!(matches!(
            attacker.dispatch(&request, &services(&host)),
            Err(Failure::Denied { detail, .. })
                if detail == "execution control is limited to processes created by this extension incarnation"
        ));
    }
    assert!(
        host.ledger.reached().is_empty(),
        "foreign IDs must be rejected before lookup, output, input, or cancellation"
    );
}

#[test]
fn pane_snapshot_fences_command_creation_but_not_its_durable_identity() {
    let host = Host::new();
    let result = session(&[Capability::TerminalProcessControl], &[]).dispatch(
        &Request::TerminalCommandStart {
            slot: "s1".into(),
            generation: 0,
            revision: 1,
            command: vec!["true".into()],
            working_directory: None,
            stdin: false,
        },
        &services(&host),
    );
    assert!(matches!(result, Err(Failure::Conflict { .. })));
    assert!(host.ledger.reached().is_empty(), "stale authority must execute nothing");

    let ownership = hl_extension::ExecutionOwnership::default();
    ownership.lock().expect("ownership").insert("e".repeat(32));

    let resumed = session(&[Capability::TerminalOutput], &[])
        .with_execution_ownership(ownership.clone())
        .dispatch(
        &Request::TerminalCommandInspect {
            id: "e".repeat(32),
            owner: COMMAND_OWNER.into(),
            slot: "s1".into(),
            generation: 0,
            revision: 1,
        },
        &services(&host),
    );
    assert!(matches!(resumed, Ok(Reply::TerminalCommand(_))));
    assert!(
        host.ledger.reached() == vec!["executions.inspect"],
        "a replacement pane must not orphan a durable command"
    );
    let output = session(&[Capability::TerminalOutput], &[])
        .with_execution_ownership(ownership)
        .dispatch(
        &Request::TerminalCommandOutput {
            id: "e".repeat(32),
            owner: COMMAND_OWNER.into(),
            slot: "s1".into(),
            generation: 0,
            revision: 1,
            after: 0,
            limit: 1,
        },
        &services(&host),
    );
    assert!(matches!(output, Ok(Reply::TerminalCommandOutput(_))));
    assert_eq!(
        host.ledger.reached(),
        vec!["executions.inspect", "executions.output"],
        "durable output must remain readable after pane replacement"
    );
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
    fn update(
        &self,
        _name: &str,
        _generation: &str,
        _configuration_revision: &str,
        configuration: &WorkspaceConfiguration,
    ) -> Result<WorkspaceConfiguration, HostError> {
        self.ledger.note("workspace.update");
        Ok(configuration.clone())
    }
    fn patch_environment(
        &self,
        _name: &str,
        generation: &str,
        configuration_revision: &str,
        _patch: &hl_extension::port::WorkspaceEnvironmentPatch,
    ) -> Result<hl_extension::port::WorkspaceEnvironmentPatchResult, HostError> {
        self.ledger.note("workspace.environment_patch");
        Ok(hl_extension::port::WorkspaceEnvironmentPatchResult {
            generation: generation.into(),
            configuration_revision: configuration_revision.into(),
            changed: true,
        })
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
    fn inventory(&self, roots: &[hl_extension::FilesystemSelector]) -> Result<FileInventory, HostError> {
        self.ledger.note("files.inventory");
        Ok(FileInventory {
            entries: vec![Entry {
                path: match &roots[0] {
                    hl_extension::FilesystemSelector::Exact { exact } => exact.clone(),
                    hl_extension::FilesystemSelector::Subtree { subtree } => subtree.clone(),
                },
                directory: true,
                size: 0,
                identity: None,
            }],
            complete: true,
            coalesced: 0,
            journal: "a".repeat(32),
            revision: 0,
        })
    }

    fn changes_since(
        &self,
        _roots: &[hl_extension::FilesystemSelector],
        _observed: &str,
        _after: u64,
        _limit: usize,
    ) -> Result<hl_extension::port::FileChangePage, HostError> {
        self.ledger.note("files.changes_since");
        Ok(hl_extension::port::FileChangePage {
            journal: "a".repeat(32),
            changes: Vec::new(),
            next: 0,
            current: 0,
            more: false,
            truncated: false,
        })
    }

    fn list(&self, path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        self.ledger.note("files.list");
        Ok(vec![Entry {
            path: path.clone(),
            directory: true,
            size: 0,
            identity: None,
        }])
    }

    fn list_page(
        &self,
        path: &RelativePath,
        _after: Option<&RelativePath>,
        _observed: Option<&str>,
        _limit: usize,
    ) -> Result<DirectoryPage, HostError> {
        self.ledger.note("files.list_page");
        Ok(DirectoryPage {
            entries: Vec::new(),
            identity: format!("directory:{}", path.as_str()),
            next: None,
            more: false,
        })
    }

    fn read(&self, _path: &RelativePath) -> Result<Vec<u8>, HostError> {
        self.ledger.note("files.read");
        Ok(b"contents".to_vec())
    }
    fn read_link(&self, _path: &RelativePath) -> Result<Vec<u8>, HostError> {
        self.ledger.note("files.read_link");
        Ok(b"app.log".to_vec())
    }
    fn read_range(
        &self,
        path: &RelativePath,
        offset: u64,
        _limit: usize,
        _observed: Option<&str>,
    ) -> Result<FileRange, HostError> {
        self.ledger.note("files.read_range");
        Ok(FileRange {
            path: path.clone(),
            identity: "v1:1:2:3:4:5:6:7".into(),
            offset,
            total: 8,
            contents: b"contents".to_vec(),
            eof: true,
            truncated: false,
        })
    }
    fn stat(&self, path: &RelativePath) -> Result<Entry, HostError> {
        self.ledger.note("files.stat");
        Ok(Entry {
            path: path.clone(),
            directory: false,
            size: 7,
            identity: None,
        })
    }

    fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
        self.ledger.note("files.write");
        Ok(())
    }
    fn write_observed(&self, _path: &RelativePath, _observed: &str, _contents: &[u8]) -> Result<String, HostError> {
        self.ledger.note("files.write_observed");
        Ok("v1:1:2:3:4:5:6:8".into())
    }
    fn create_observed(&self, _path: &RelativePath, _contents: &[u8]) -> Result<String, HostError> {
        self.ledger.note("files.create_observed");
        Ok("v1:1:2:3:4:5:6:7".into())
    }

    fn mkdir(&self, _path: &RelativePath) -> Result<(), HostError> {
        self.ledger.note("files.mkdir");
        Ok(())
    }

    fn rename(&self, _from: &RelativePath, _to: &RelativePath) -> Result<(), HostError> {
        self.ledger.note("files.rename");
        Ok(())
    }
    fn rename_observed(&self, _from: &RelativePath, _to: &RelativePath, _observed: &str) -> Result<String, HostError> {
        self.ledger.note("files.rename_observed");
        Ok("v1:1:2:3:4:5:6:8".into())
    }

    fn remove(&self, _path: &RelativePath) -> Result<(), HostError> {
        self.ledger.note("files.remove");
        Ok(())
    }
    fn remove_observed(&self, _path: &RelativePath, _observed: &str) -> Result<(), HostError> {
        self.ledger.note("files.remove_observed");
        Ok(())
    }
}

impl ExtensionStore for Host {
    fn catalogue(&self) -> Result<hl_extension::port::ExtensionCatalogue, HostError> {
        self.ledger.note("extensions.catalogue");
        Ok(hl_extension::port::ExtensionCatalogue {
            entries: Vec::new(),
            complete: true,
        })
    }

    fn list(&self) -> Result<Vec<ExtensionSummary>, HostError> {
        self.ledger.note("extensions.list");
        Ok(vec![ExtensionSummary {
            name: "sample".into(),
            image_digest: "sha256:abc".into(),
            status: "duty".into(),
            version: "1.0.0".into(),
            enabled: true,
            pane_providers: Vec::new(),
            granted: Grant::default(),
            images: hl_extension::ImageGrant::default(),
            containers: hl_extension::ContainerGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        }])
    }
    fn inspect(&self, name: &str) -> Result<ExtensionSummary, HostError> {
        self.ledger.note("extensions.inspect");
        Ok(ExtensionSummary {
            name: name.into(),
            image_digest: "sha256:abc".into(),
            status: "duty".into(),
            version: "1.0.0".into(),
            enabled: true,
            pane_providers: Vec::new(),
            granted: Grant::default(),
            images: hl_extension::ImageGrant::default(),
            containers: hl_extension::ContainerGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
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
    fn retry(&self, _name: &str, _image_digest: &str) -> Result<(), HostError> {
        self.ledger.note("extensions.retry");
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
    fn install(
        &self,
        job: &str,
        _revision: u64,
        _image_digest: &str,
        _granted: &Grant,
        _containers: &hl_extension::ContainerGrant,
        _images: &hl_extension::ImageGrant,
        _networks: &hl_extension::NetworkGrant,
        _volumes: &hl_extension::VolumeGrant,
        _filesystem: &hl_extension::FilesystemGrant,
        _workspace_environment: &hl_extension::WorkspaceEnvironmentGrant,
    ) -> Result<ExtensionSummary, HostError> {
        self.ledger.note("extensions.install");
        ExtensionStore::inspect(self, job)
    }
    fn update(
        &self,
        job: &str,
        _revision: u64,
        _image_digest: &str,
        _granted: &Grant,
        _containers: &hl_extension::ContainerGrant,
        _images: &hl_extension::ImageGrant,
        _networks: &hl_extension::NetworkGrant,
        _volumes: &hl_extension::VolumeGrant,
        _filesystem: &hl_extension::FilesystemGrant,
        _workspace_environment: &hl_extension::WorkspaceEnvironmentGrant,
    ) -> Result<ExtensionSummary, HostError> {
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
        state: host,
        notifications: host,
    }
}

struct CredentialPort {
    read: Cell<bool>,
}

impl ExtensionStateStore for CredentialPort {
    fn read(&self) -> Result<ExtensionState, HostError> {
        unreachable!()
    }
    fn write(&self, _observed: &str, _contents: &[u8]) -> Result<String, HostError> {
        unreachable!()
    }
    fn clear(&self, _observed: &str) -> Result<(), HostError> {
        unreachable!()
    }
    fn credential(&self, key: &str) -> Result<ExtensionCredential, HostError> {
        self.read.set(true);
        assert_eq!(key, "postgres.password");
        Ok(ExtensionCredential {
            key: key.to_owned(),
            revision: 1,
            value: Some(b"sentinel-password".to_vec()),
        })
    }
}

struct MismatchedCredentialPort;

impl ExtensionStateStore for MismatchedCredentialPort {
    fn read(&self) -> Result<ExtensionState, HostError> {
        unreachable!()
    }
    fn write(&self, _observed: &str, _contents: &[u8]) -> Result<String, HostError> {
        unreachable!()
    }
    fn clear(&self, _observed: &str) -> Result<(), HostError> {
        unreachable!()
    }
    fn credential(&self, _key: &str) -> Result<ExtensionCredential, HostError> {
        Ok(ExtensionCredential {
            key: "production.password".into(),
            revision: 1,
            value: Some(b"secret".to_vec()),
        })
    }
}

fn services_with_state<'a>(host: &'a Host, state: &'a dyn ExtensionStateStore) -> Services<'a> {
    let mut services = services(host);
    services.state = state;
    services
}

fn session(capabilities: &[Capability], roots: &[&str]) -> Session {
    let roots: Vec<_> = roots
        .iter()
        .map(|root| RelativePath::new(*root).expect("root"))
        .collect();
    let selectors: Vec<_> = roots
        .iter()
        .cloned()
        .map(|subtree| hl_extension::FilesystemSelector::Subtree { subtree })
        .collect();
    Session::new(Authority::new(
        ExtensionName::new("sample").expect("name"),
        Grant::new(capabilities.iter().copied()),
        roots.clone(),
    ))
    .with_extension_identity(COMMAND_OWNER)
    .with_containers(hl_extension::ContainerGrant {
        selectors: vec![hl_extension::ContainerSelector::All { all: true }],
        create: true,
    })
    .with_images(hl_extension::ImageGrant {
        read: vec![hl_extension::ImageSelector::All { all: true }],
        r#use: vec![hl_extension::ImageSelector::All { all: true }],
        pull: vec![hl_extension::ImageSelector::All { all: true }],
        remove: vec![hl_extension::ImageSelector::All { all: true }],
        prune_all_unused: true,
    })
    .with_networks(hl_extension::NetworkGrant {
        selectors: vec![hl_extension::NetworkSelector::All { all: true }],
        create: true,
    })
    .with_volumes(hl_extension::VolumeGrant {
        selectors: vec![hl_extension::VolumeSelector::All { all: true }],
        create: true,
    })
    .with_filesystem(hl_extension::FilesystemGrant {
        read: selectors.clone(),
        write: selectors.clone(),
        create: selectors.clone(),
        delete: selectors.clone(),
        rename: selectors,
    })
}

fn path(value: &str) -> RelativePath {
    RelativePath::new(value).expect("path")
}

fn workspace_configuration() -> WorkspaceConfiguration {
    WorkspaceConfiguration {
        generation: "0123456789abcdef0123456789abcdef".into(),
        configuration_revision: "abcdef0123456789abcdef0123456789".into(),
        name: "other".into(),
        image: "docker.io/library/alpine:3.20".into(),
        architecture: "arm64".into(),
        storage: None,
        shell: None,
        cpus: None,
        memory_mb: None,
        environment: vec![("DATABASE_PASSWORD".into(), "cycle19-secret".into())],
        environment_redacted: false,
        mounts: Vec::new(),
        docker_socket: true,
        scrollback: Some(100_000),
        vpn: None,
        execution_lifetime: "persisted".into(),
        terminal: WorkspaceTerminal::default(),
    }
}

#[test]
fn workspace_inspection_always_redacts_environment_values() {
    let host = Host::new();
    let request = Request::WorkspaceInspect { name: "other".into() };

    let failure = session(&[], &[])
        .dispatch(&request, &services(&host))
        .expect_err("inspection requires authority");
    assert!(matches!(failure, Failure::Denied { .. }));
    assert!(host.ledger.reached().is_empty(), "denial must precede host inspection");

    let reply = session(&[Capability::WorkspaceRead], &[])
        .dispatch(&request, &services(&host))
        .expect("read-only inspection");
    let Reply::WorkspaceConfiguration(configuration) = reply else {
        panic!("unexpected reply")
    };
    assert!(configuration.environment.is_empty());
    assert!(configuration.environment_redacted);
    assert!(!format!("{configuration:?}").contains("cycle19-secret"));

    let reply = session(&[Capability::WorkspaceRead, Capability::WorkspaceControl], &[])
        .dispatch(&request, &services(&host))
        .expect("explicit control authority");
    let Reply::WorkspaceConfiguration(configuration) = reply else {
        panic!("unexpected reply")
    };
    assert!(configuration.environment.is_empty());
    assert!(configuration.environment_redacted);
}

#[test]
fn workspace_environment_grant_filters_by_exact_workspace_and_name() {
    let host = Host::new();
    let authority = Authority::new(
        ExtensionName::new("sample").unwrap(),
        Grant::new([Capability::WorkspaceRead, Capability::WorkspaceEnvironmentRead]),
        Vec::new(),
    );
    let mut scoped = Session::new(authority).with_workspace_environment(hl_extension::WorkspaceEnvironmentGrant {
        read: vec![hl_extension::WorkspaceEnvironmentSelector::Exact {
            workspace: "other".into(),
            name: "DATABASE_PASSWORD".into(),
        }],
        write: Vec::new(),
    });
    let reply = scoped
        .dispatch(&Request::WorkspaceInspect { name: "other".into() }, &services(&host))
        .expect("exact grant");
    let Reply::WorkspaceConfiguration(configuration) = reply else {
        panic!("unexpected reply")
    };
    assert_eq!(
        configuration.environment,
        vec![("DATABASE_PASSWORD".into(), "cycle19-secret".into())]
    );
    assert!(!configuration.environment_redacted);

    let reply = scoped
        .dispatch(&Request::WorkspaceInspect { name: "sibling".into() }, &services(&host))
        .expect("wrong workspace remains inspectable but secret-free");
    let Reply::WorkspaceConfiguration(configuration) = reply else {
        panic!("unexpected reply")
    };
    assert!(configuration.environment.is_empty());
    assert!(configuration.environment_redacted);
}

#[test]
fn workspace_creation_cannot_bypass_environment_write_consent() {
    let host = Host::new();
    let request = Request::WorkspaceCreate {
        configuration: workspace_configuration(),
    };
    let failure = session(&[Capability::WorkspaceControl], &[])
        .dispatch(&request, &services(&host))
        .expect_err("lifecycle control does not grant environment injection");
    assert!(matches!(failure, Failure::Denied { ref capability, .. }
        if capability == Capability::WorkspaceEnvironmentWrite.as_str()));
    assert!(host.ledger.reached().is_empty(), "denial must precede creation");

    let authority = || Authority::new(
        ExtensionName::new("sample").unwrap(),
        Grant::new([Capability::WorkspaceControl, Capability::WorkspaceEnvironmentWrite]),
        Vec::new(),
    );
    let mut wrong_workspace = Session::new(authority()).with_workspace_environment(
        hl_extension::WorkspaceEnvironmentGrant {
            read: Vec::new(),
            write: vec![hl_extension::WorkspaceEnvironmentSelector::Exact {
                workspace: "sibling".into(),
                name: "DATABASE_PASSWORD".into(),
            }],
        },
    );
    assert!(matches!(wrong_workspace.dispatch(&request, &services(&host)), Err(Failure::Denied { .. })));
    assert!(host.ledger.reached().is_empty(), "wrong selector must precede creation");

    let mut scoped = Session::new(authority()).with_workspace_environment(
        hl_extension::WorkspaceEnvironmentGrant {
            read: Vec::new(),
            write: vec![hl_extension::WorkspaceEnvironmentSelector::Exact {
                workspace: "other".into(),
                name: "DATABASE_PASSWORD".into(),
            }],
        },
    );
    let Reply::WorkspaceConfiguration(created) = scoped
        .dispatch(&request, &services(&host))
        .expect("independently consented initial environment") else { panic!("unexpected reply") };
    assert_eq!(host.ledger.reached(), vec!["workspace.create"]);
    assert!(created.environment.is_empty(), "write authority must not imply secret readback");
    assert!(created.environment_redacted);
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
                configuration: WorkspaceConfiguration {
                    environment: Vec::new(),
                    ..workspace_configuration()
                },
            },
            Capability::WorkspaceControl,
        ),
        (
            Request::WorkspaceUpdate {
                name: "other".into(),
                generation: "0123456789abcdef0123456789abcdef".into(),
                configuration_revision: "abcdef0123456789abcdef0123456789".into(),
                configuration: WorkspaceConfiguration {
                    environment: Vec::new(),
                    ..workspace_configuration()
                },
            },
            Capability::WorkspaceConfigure,
        ),
        (
            Request::WorkspaceEnvironmentPatch {
                name: "other".into(),
                generation: "0123456789abcdef0123456789abcdef".into(),
                configuration_revision: "abcdef0123456789abcdef0123456789".into(),
                patch: hl_extension::port::WorkspaceEnvironmentPatch {
                    set: vec![("TOKEN".into(), "value".into())],
                    remove: Vec::new(),
                },
            },
            Capability::WorkspaceEnvironmentWrite,
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
        (Request::ExtensionCatalogue, Capability::ExtensionRead),
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
            Request::ExtensionRetry {
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
            Capability::ExtensionRemove,
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
            Request::NotificationPublish {
                notification: hl_extension::Notification {
                    id: "build".into(),
                    title: "Complete".into(),
                    body: "done".into(),
                },
            },
            Capability::NotificationPublish,
        ),
        (
            Request::ExtensionInstall {
                image_digest: format!("sha256:{}", "a".repeat(64)),
                job: "job-1".into(),
                revision: 7,
                granted: Grant::new([Capability::Interface]),
                containers: hl_extension::ContainerGrant::default(),
                images: hl_extension::ImageGrant::default(),
                networks: hl_extension::NetworkGrant::default(),
                volumes: hl_extension::VolumeGrant::default(),
                filesystem: hl_extension::FilesystemGrant::default(),
                workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
            },
            Capability::ExtensionInstall,
        ),
        (
            Request::ExtensionUpdate {
                image_digest: format!("sha256:{}", "a".repeat(64)),
                job: "job-1".into(),
                revision: 7,
                granted: Grant::new([Capability::Interface]),
                images: hl_extension::ImageGrant::default(),
                networks: hl_extension::NetworkGrant::default(),
                volumes: hl_extension::VolumeGrant::default(),
                containers: hl_extension::ContainerGrant::default(),
                filesystem: hl_extension::FilesystemGrant::default(),
                workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
            },
            Capability::ExtensionInstall,
        ),
        (Request::ContainerList, Capability::ContainerRead),
        (Request::ContainerInspect { id: "c1".into() }, Capability::ContainerRead),
        (
            Request::ContainerInspectObserved {
                id: "c1".into(),
                generation: 0,
            },
            Capability::ContainerRead,
        ),
        (
            Request::ContainerProcesses {
                id: "c1".into(),
                snapshot: None,
                after: 0,
                limit: 128,
            },
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
        (
            Request::ExecutionInspect { id: "e".repeat(32) },
            Capability::ContainerRead,
        ),
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
            Request::ExecutionOutput {
                id: "e".repeat(32),
                after: 0,
                limit: 16,
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
                    image: "docker.io/library/alpine:latest".into(),
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
            Capability::ContainerCreate,
        ),
        (
            Request::ContainerStart {
                id: "c".repeat(64),
                generation: 4,
            },
            Capability::ContainerLifecycle,
        ),
        (
            Request::ContainerStop {
                id: "c".repeat(64),
                generation: 4,
            },
            Capability::ContainerLifecycle,
        ),
        (
            Request::ContainerRemove {
                id: "c".repeat(64),
                generation: 4,
            },
            Capability::ContainerRemove,
        ),
        (
            Request::ContainerPause {
                id: "c".repeat(64),
                generation: 4,
            },
            Capability::ContainerLifecycle,
        ),
        (
            Request::ContainerUnpause {
                id: "c".repeat(64),
                generation: 4,
            },
            Capability::ContainerLifecycle,
        ),
        (
            Request::ContainerRestart {
                id: "c".repeat(64),
                generation: 4,
            },
            Capability::ContainerLifecycle,
        ),
        (
            Request::ContainerRename {
                id: "c".repeat(64),
                generation: 4,
                name: "worker-2".into(),
            },
            Capability::ContainerLifecycle,
        ),
        (
            Request::ContainerKill {
                id: "c".repeat(64),
                generation: 4,
                signal: "SIGTERM".into(),
            },
            Capability::ContainerLifecycle,
        ),
        (
            Request::ExecutionKill {
                id: "e".repeat(32),
                signal: "SIGTERM".into(),
            },
            Capability::ContainerExecute,
        ),
        (
            Request::ExecutionCancel {
                id: "e".repeat(32),
                signal: "SIGTERM".into(),
                timeout_ms: 500,
            },
            Capability::ContainerExecute,
        ),
        (
            Request::ExecutionRemove { id: "e".repeat(32) },
            Capability::ContainerExecute,
        ),
        (
            Request::ContainerExec {
                environment: Vec::new(),
                id: "c".repeat(64),
                generation: 4,
                command: vec!["worker".into()],
                user: None,
                working_directory: None,
                stdin: false,
            },
            Capability::ContainerExecute,
        ),
        (Request::ImageList, Capability::ImageRead),
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
            Capability::ImageRemove,
        ),
        (Request::ImagePrune, Capability::ImagePrune),
        (Request::TerminalTabs, Capability::TerminalRead),
        (Request::TerminalTopology, Capability::TerminalRead),
        (Request::PaneList, Capability::PaneObserve),
        (
            Request::TerminalOpenTab { title: "logs".into() },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalPinTab {
                tab: "t1".into(),
                pinned: true,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalFocusTab { tab: "t1".into() },
            Capability::TerminalFocus,
        ),
        (
            Request::TerminalSplit {
                slot: "s1".into(),
                division: Division::Beside,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalSplitObserved {
                slot: "s1".into(),
                generation: 7,
                revision: 11,
                division: Division::Below,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalSpawn {
                slot: "s1".into(),
                command: vec!["ls".into()],
            },
            Capability::TerminalProcessControl,
        ),
        (
            Request::TerminalSpawnObserved {
                slot: "s1".into(),
                generation: 7,
                revision: 11,
                command: vec!["ls".into()],
            },
            Capability::TerminalProcessControl,
        ),
        (
            Request::TerminalCommandStart {
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                command: vec!["true".into()],
                working_directory: None,
                stdin: false,
            },
            Capability::TerminalProcessControl,
        ),
        (
            Request::TerminalCommandInspect {
                id: "e".repeat(32),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
            },
            Capability::TerminalOutput,
        ),
        (
            Request::TerminalCommandOutput {
                id: "e".repeat(32),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                after: 0,
                limit: 1,
            },
            Capability::TerminalOutput,
        ),
        (
            Request::TerminalCommandWait {
                id: "e".repeat(32),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                timeout_ms: 1,
            },
            Capability::TerminalOutput,
        ),
        (
            Request::TerminalCommandCancel {
                id: "e".repeat(32),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                signal: "SIGTERM".into(),
                timeout_ms: 1,
            },
            Capability::TerminalProcessControl,
        ),
        (
            Request::TerminalCommandWrite {
                id: "e".repeat(32),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
                contents: vec![1],
            },
            Capability::TerminalInput,
        ),
        (
            Request::TerminalCommandCloseInput {
                id: "e".repeat(32),
                owner: COMMAND_OWNER.into(),
                slot: "s1".into(),
                generation: 0,
                revision: 0,
            },
            Capability::TerminalInput,
        ),
        (
            Request::TerminalWritePane {
                slot: "s1".into(),
                generation: 1,
                revision: 2,
                contents: b"pwd\n".to_vec(),
            },
            Capability::TerminalInput,
        ),
        (
            Request::TerminalResizeGrid {
                slot: "s1".into(),
                columns: 120,
                rows: 40,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalRetitlePane {
                slot: "s1".into(),
                title: "Build 🧪".into(),
            },
            Capability::TerminalLayoutControl,
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
            Capability::TerminalProcessControl,
        ),
        (
            Request::TerminalSwitchOccupantObserved {
                slot: "s1".into(),
                generation: 7,
                revision: 11,
                target: hl_extension::port::PaneOccupantTarget::Terminal,
            },
            Capability::TerminalProcessControl,
        ),
        (Request::FilesystemInventory, Capability::FilesystemRead),
        (
            Request::FilesystemList { path: path("logs") },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemListPage {
                path: path("logs"),
                after: None,
                observed: None,
                limit: 2,
            },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemRead {
                path: path("logs/app.log"),
            },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemReadLink {
                path: path("logs/current"),
            },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemReadRange {
                path: path("logs/app.log"),
                offset: 0,
                limit: 8,
                observed: None,
            },
            Capability::FilesystemRead,
        ),
        (
            Request::FilesystemReadRanges {
                ranges: vec![FileRangeRequest {
                    path: path("logs/app.log"),
                    offset: 0,
                    limit: 8,
                    observed: None,
                }],
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
            Request::FilesystemWriteObserved {
                path: path("logs/app.log"),
                observed: "v1:1:2:3:4:5:6:7".into(),
                contents: b"x".to_vec(),
            },
            Capability::FilesystemWrite,
        ),
        (
            Request::FilesystemCreateObserved {
                path: path("logs/new.log"),
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
            Request::FilesystemRenameObserved {
                from: path("logs/a"),
                to: path("logs/b"),
                observed: "v1:1:2:3:4:5:6:7".into(),
            },
            Capability::FilesystemWrite,
        ),
        (
            Request::FilesystemRemove { path: path("logs/old") },
            Capability::FilesystemWrite,
        ),
        (
            Request::FilesystemRemoveObserved {
                path: path("logs/old"),
                observed: "v1:1:2:3:4:5:6:7".into(),
            },
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

/// Every authoritative request variant. `calls` is the subset whose happy
/// path is independent of prior session state; this inventory also includes
/// stateful interface calls and the remaining resource operations so denial
/// is proven before any authority-bearing port is reached.
fn all_calls() -> Vec<(Request, Capability)> {
    let mut requests = calls();
    requests.extend([
        (
            Request::FilesystemChanges {
                observed: "a".repeat(32),
                after: 0,
                limit: 1,
            },
            Capability::FilesystemRead,
        ),
        (Request::StateRead, Capability::StateRead),
        (
            Request::StateWrite {
                observed: "absent".into(),
                contents: vec![0, 17, 255],
            },
            Capability::StateWrite,
        ),
        (
            Request::StateClear {
                observed: "absent".into(),
            },
            Capability::StateWrite,
        ),
        (Request::PreferenceRead, Capability::PreferenceRead),
        (
            Request::PreferenceSet {
                observed: 0,
                key: "sidebar-width".into(),
                value: PreferenceValue::Number(240),
            },
            Capability::PreferenceWrite,
        ),
        (
            Request::PreferenceRemove {
                observed: 0,
                key: "sidebar-width".into(),
            },
            Capability::PreferenceWrite,
        ),
        (
            Request::CredentialRead {
                key: "postgres.password".into(),
            },
            Capability::CredentialRead,
        ),
        (
            Request::CredentialSet {
                observed: 0,
                key: "postgres.password".into(),
                value: vec![0, 255],
            },
            Capability::CredentialWrite,
        ),
        (
            Request::CredentialRemove {
                observed: 0,
                key: "postgres.password".into(),
            },
            Capability::CredentialWrite,
        ),
        (
            Request::ContainerExecCredential {
                id: "a".repeat(64),
                generation: 1,
                command: vec!["psql".into()],
                environment: Vec::new(),
                credentials: vec![("PGPASSWORD".into(), "postgres.password".into())],
                user: None,
                working_directory: None,
                stdin: false,
            },
            Capability::ContainerExecute,
        ),
        (
            Request::ExecutionWrite {
                id: "e".repeat(32),
                contents: vec![1],
            },
            Capability::ContainerInput,
        ),
        (
            Request::ExecutionCloseInput { id: "e".repeat(32) },
            Capability::ContainerInput,
        ),
    ]);
    requests.extend([
        (
            Request::ContainerAttachTerminal {
                id: "c".repeat(64),
                command: vec!["sh".into()],
            },
            Capability::ContainerAttach,
        ),
        (
            Request::ImagePullStart {
                reference: "alpine".into(),
            },
            Capability::ImagePull,
        ),
        (Request::ImagePullStatus { job: "job".into() }, Capability::ImagePull),
        (Request::ImagePullCancel { job: "job".into() }, Capability::ImagePull),
        (Request::VolumeList, Capability::VolumeRead),
        (Request::VolumeInspect { name: "cache".into() }, Capability::VolumeRead),
        (Request::VolumeCreate { name: "cache".into() }, Capability::VolumeWrite),
        (
            Request::VolumeRemove {
                name: "cache".into(),
                generation: "a".repeat(32),
            },
            Capability::VolumeWrite,
        ),
        (Request::NetworkList, Capability::NetworkRead),
        (
            Request::NetworkInspect {
                reference: "bridge".into(),
            },
            Capability::NetworkRead,
        ),
        (
            Request::NetworkCreate { name: "private".into() },
            Capability::NetworkWrite,
        ),
        (
            Request::NetworkRemove {
                reference: "private".into(),
            },
            Capability::NetworkWrite,
        ),
        (
            Request::NetworkConnect {
                reference: "private".into(),
                container: "c".repeat(64),
                aliases: Vec::new(),
            },
            Capability::NetworkWrite,
        ),
        (
            Request::NetworkDisconnect {
                reference: "private".into(),
                container: "c".repeat(64),
            },
            Capability::NetworkWrite,
        ),
        (
            Request::TerminalReadPane {
                slot: "s1".into(),
                lines: None,
            },
            Capability::TerminalOutput,
        ),
        (
            Request::PaneSemanticRead { slot: "s1".into() },
            Capability::PaneSemanticRead,
        ),
        (
            Request::PaneSemanticAction {
                slot: "s1".into(),
                action: PaneSemanticAction {
                    generation: 0,
                    revision: 1,
                    node: 1,
                    action: SemanticActionKind::Invoke,
                    value: None,
                },
            },
            Capability::PaneSemanticControl,
        ),
        (
            Request::TerminalResizeGridObserved {
                slot: "s1".into(),
                generation: 0,
                revision: 1,
                columns: 80,
                rows: 24,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalClosePane { slot: "s1".into() },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalClosePaneObserved {
                slot: "s1".into(),
                generation: 0,
                revision: 1,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalFocusPane { slot: "s1".into() },
            Capability::TerminalFocus,
        ),
        (
            Request::TerminalFocusPaneObserved {
                slot: "s1".into(),
                generation: 0,
                revision: 1,
            },
            Capability::TerminalFocus,
        ),
        (
            Request::TerminalRetitlePaneObserved {
                slot: "s1".into(),
                generation: 0,
                revision: 1,
                title: "Build".into(),
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalRatio {
                slot: "s1".into(),
                ratio: 0.5,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::TerminalRatioObserved {
                slot: "s1".into(),
                generation: 0,
                revision: 1,
                ratio: 0.5,
            },
            Capability::TerminalLayoutControl,
        ),
        (
            Request::InterfaceSplit {
                slot: "s1".into(),
                division: Division::Beside,
            },
            Capability::Interface,
        ),
        (Request::InterfaceWithdraw { slot: "s1".into() }, Capability::Interface),
        (
            Request::InterfaceRender {
                frame: hl_gui::Frame::new(0),
            },
            Capability::Interface,
        ),
        (
            Request::InterfaceRenderAt {
                slot: "s1".into(),
                frame: hl_gui::Frame::new(0),
            },
            Capability::Interface,
        ),
        (
            Request::SourceResize {
                mutation: hl_gui::SourceMutation::Length {
                    source: hl_gui::SourceId::new(1),
                    version: hl_gui::Version::new(1),
                    rows: 1,
                },
            },
            Capability::Interface,
        ),
        (
            Request::SourceResizeAt {
                slot: "s1".into(),
                mutation: hl_gui::SourceMutation::Length {
                    source: hl_gui::SourceId::new(1),
                    version: hl_gui::Version::new(1),
                    rows: 1,
                },
            },
            Capability::Interface,
        ),
        (
            Request::EventSubscribe {
                topic: Topic::WorkspaceEvents,
            },
            Capability::WorkspaceEvents,
        ),
        (
            Request::EventUnsubscribe {
                topic: Topic::WorkspaceEvents,
            },
            Capability::WorkspaceEvents,
        ),
    ]);
    requests
}

#[test]
fn every_authoritative_request_has_one_explicit_capability_and_is_denied_before_work() {
    let specification: serde_json::Value =
        serde_json::from_str(&hl_extension::specification::document()).expect("specification");
    let authoritative = specification["roots"]["request"]["variants"]
        .as_array()
        .expect("request variants")
        .iter()
        .map(|variant| variant["name"].as_str().expect("variant name").to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let requests = all_calls();
    let represented = requests
        .iter()
        .map(|(request, _)| {
            serde_json::to_value(request).expect("request JSON")["call"]
                .as_str()
                .expect("call tag")
                .to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(represented, authoritative, "authority inventory must exhaust Request");
    assert_eq!(
        requests.len(),
        represented.len(),
        "each Request variant must appear exactly once"
    );

    for (request, capability) in requests {
        assert_eq!(request.capability(), capability, "wrong authority for {request:?}");
        let host = Host::new();
        let mut denied = session(&[], &["logs"]);
        assert!(matches!(
            denied.dispatch(&request, &services(&host)),
            Err(Failure::Denied { .. })
        ));
        assert!(
            host.ledger.reached().is_empty(),
            "{request:?} reached authority before denial"
        );
    }
}

#[test]
fn every_call_succeeds_with_its_capability_and_fails_without_it() {
    for (request, capability) in calls() {
        let host = Host::new();

        let mut capabilities = vec![capability];
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
            capabilities.push(Capability::TerminalLayoutControl);
            capabilities.push(Capability::TerminalProcessControl);
        }
        let ownership = hl_extension::ExecutionOwnership::default();
        ownership
            .lock()
            .expect("ownership")
            .insert("e".repeat(32));
        let mut granted = session(&capabilities, &["logs"])
            .with_execution_ownership(ownership)
            .with_images(hl_extension::ImageGrant {
                read: vec![hl_extension::ImageSelector::All { all: true }],
                r#use: vec![hl_extension::ImageSelector::All { all: true }],
                pull: vec![hl_extension::ImageSelector::All { all: true }],
                remove: vec![hl_extension::ImageSelector::All { all: true }],
                prune_all_unused: true,
            })
            .with_workspace_environment(hl_extension::WorkspaceEnvironmentGrant {
                read: Vec::new(),
                write: vec![hl_extension::WorkspaceEnvironmentSelector::All { all: true }],
            });
        if matches!(
            request,
            Request::ExecutionKill { .. } | Request::ExecutionCancel { .. } | Request::ExecutionRemove { .. }
        ) {
            granted
                .dispatch(
                    &Request::ContainerExec {
                        id: "c".repeat(64),
                        generation: 4,
                        command: vec!["true".into()],
                        environment: Vec::new(),
                        user: None,
                        working_directory: None,
                        stdin: false,
                    },
                    &services(&host),
                )
                .unwrap();
        }
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
    let update = Request::WorkspaceUpdate {
        name: "other".into(),
        generation: "short".into(),
        configuration_revision: "abcdef0123456789abcdef0123456789".into(),
        configuration: WorkspaceConfiguration {
            environment: Vec::new(),
            ..workspace_configuration()
        },
    };
    assert!(
        session(&[Capability::WorkspaceConfigure], &[])
            .dispatch(&update, &services(&host))
            .is_err()
    );
    let delete = Request::WorkspaceDelete {
        name: "other".into(),
        generation: String::new(),
    };
    assert!(
        session(&[Capability::WorkspaceControl], &[])
            .dispatch(&delete, &services(&host))
            .is_err()
    );
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn workspace_settings_update_cannot_bypass_exact_environment_patch_authority() {
    let host = Host::new();
    let request = Request::WorkspaceUpdate {
        name: "other".into(),
        generation: "0123456789abcdef0123456789abcdef".into(),
        configuration_revision: "abcdef0123456789abcdef0123456789".into(),
        configuration: workspace_configuration(),
    };
    let authority = Authority::new(
        ExtensionName::new("sample").unwrap(),
        Grant::new([
            Capability::WorkspaceConfigure,
            Capability::WorkspaceEnvironmentWrite,
        ]),
        Vec::new(),
    );
    let mut session = Session::new(authority).with_workspace_environment(
        hl_extension::WorkspaceEnvironmentGrant {
            read: Vec::new(),
            write: vec![hl_extension::WorkspaceEnvironmentSelector::Exact {
                workspace: "other".into(),
                name: "DATABASE_PASSWORD".into(),
            }],
        },
    );

    let failure = session
        .dispatch(&request, &services(&host))
        .expect_err("general settings update must not carry environment values");
    assert!(matches!(failure, Failure::Conflict { ref detail }
        if detail.contains("workspace_environment_patch")));
    assert!(host.ledger.reached().is_empty(), "rejection must precede the host");
}

#[test]
fn extension_acquisition_identifiers_are_bounded_before_the_host() {
    let host = Host::new();
    let mut session = session(&[Capability::ExtensionInstall], &[]);
    assert!(
        session
            .dispatch(
                &Request::ExtensionAcquisitionStart {
                    reference: "x".repeat(513)
                },
                &services(&host)
            )
            .is_err()
    );
    assert!(
        session
            .dispatch(
                &Request::ExtensionAcquisitionStart {
                    reference: "bad reference".into()
                },
                &services(&host)
            )
            .is_err()
    );
    assert!(
        session
            .dispatch(
                &Request::ExtensionAcquisitionStatus { job: "x".repeat(129) },
                &services(&host)
            )
            .is_err()
    );
    assert!(matches!(
        session.dispatch(
            &Request::ExtensionInstall {
                job: "job-1".into(),
                revision: 7,
                image_digest: "sha256:stale-catalogue-label".into(),
                granted: Grant::default(),
                containers: hl_extension::ContainerGrant::default(),
                images: hl_extension::ImageGrant::default(),
                networks: hl_extension::NetworkGrant::default(),
                volumes: hl_extension::VolumeGrant::default(),
                filesystem: hl_extension::FilesystemGrant::default(),
                workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
            },
            &services(&host),
        ),
        Err(Failure::Conflict { .. })
    ));
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
    let mut session = session(&[Capability::ExtensionControl, Capability::ExtensionRemove], &[]);
    for request in [
        Request::ExtensionEnable {
            name: "sample".into(),
            image_digest: "sha256:abc".into(),
        },
        Request::ExtensionDisable {
            name: "sample".into(),
            image_digest: String::new(),
        },
        Request::ExtensionRetry {
            name: "sample".into(),
            image_digest: "sha256:abc".into(),
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
fn filesystem_journal_identity_is_bounded_before_the_host_is_reached() {
    let host = Host::new();
    let mut session = session(&[Capability::FilesystemRead], &["src"]);
    for observed in ["not-hex".to_owned(), "a".repeat(33)] {
        assert!(matches!(
            session.dispatch(
                &Request::FilesystemChanges {
                    observed,
                    after: 0,
                    limit: 1
                },
                &services(&host),
            ),
            Err(Failure::Failed { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn terminal_input_and_grid_are_bounded_before_the_window_is_reached() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalInput, Capability::TerminalLayoutControl], &[]);
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
fn terminal_input_alone_cannot_reach_process_or_layout_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalInput], &[]);
    let requests = [
        Request::TerminalOpenTab { title: "logs".into() },
        Request::TerminalPinTab {
            tab: "t1".into(),
            pinned: true,
        },
        Request::TerminalSplit {
            slot: "s1".into(),
            division: Division::Beside,
        },
        Request::TerminalSpawn {
            slot: "s1".into(),
            command: vec!["sh".into()],
        },
        Request::TerminalResizeGrid {
            slot: "s1".into(),
            columns: 80,
            rows: 24,
        },
        Request::TerminalClosePane { slot: "s1".into() },
        Request::TerminalFocusPane { slot: "s1".into() },
        Request::TerminalRetitlePane {
            slot: "s1".into(),
            title: "logs".into(),
        },
        Request::TerminalRatio {
            slot: "s1".into(),
            ratio: 0.5,
        },
    ];
    for request in requests {
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Denied { capability, .. })
                if capability == request.capability().as_str()
        ));
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn terminal_focus_can_select_a_tab_that_has_no_focusable_pane() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalFocus], &[]);
    let reply = session
        .dispatch(
            &Request::TerminalFocusTab { tab: "t1".into() },
            &services(&host),
        )
        .expect("focus tab");
    assert_eq!(reply, Reply::Done);
    assert_eq!(host.ledger.reached(), vec!["terminal.focus_tab"]);
}

#[test]
fn process_lifetime_layout_operations_require_both_grants_before_host_access() {
    for granted in [Capability::TerminalLayoutControl, Capability::TerminalProcessControl] {
        for request in [
            Request::TerminalOpenTab { title: "shell".into() },
            Request::TerminalSplit {
                slot: "s1".into(),
                division: Division::Beside,
            },
            Request::TerminalClosePane { slot: "s1".into() },
            Request::TerminalSwitchOccupant {
                slot: "s1".into(),
                generation: 7,
                target: hl_extension::port::PaneOccupantTarget::Terminal,
            },
        ] {
            let host = Host::new();
            let mut session = session(&[granted], &[]);
            assert!(matches!(
                session.dispatch(&request, &services(&host)),
                Err(Failure::Denied { .. })
            ));
            assert!(host.ledger.reached().is_empty());
        }
    }
}

#[test]
fn pane_titles_are_utf8_bounded_and_refused_before_terminal_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalLayoutControl], &[]);
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
fn terminal_focus_grant_cannot_mutate_layout() {
    let host = Host::new();
    let mut session = session(&[Capability::TerminalFocus], &[]);
    assert!(session
        .dispatch(&Request::TerminalFocusPane { slot: "s1".into() }, &services(&host))
        .is_ok());
    assert_eq!(host.ledger.reached(), ["terminal.focus"]);
    host.ledger.clear();
    assert!(matches!(
        session.dispatch(&Request::TerminalClosePane { slot: "s1".into() }, &services(&host)),
        Err(Failure::Denied { capability, .. }) if capability == "terminals:layout-control"
    ));
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn occupant_targets_are_native_names_and_reach_terminal_authority_exactly_once() {
    let host = Host::new();
    let mut session = session(
        &[Capability::TerminalLayoutControl, Capability::TerminalProcessControl],
        &[],
    );
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
    let mut session = session(&[Capability::TerminalProcessControl], &[]);
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
            Capability::ContainerCreate,
            Capability::VolumeRead,
            Capability::NetworkWrite,
        ],
        &[],
    )
    .with_images(hl_extension::ImageGrant {
        r#use: vec![hl_extension::ImageSelector::Reference {
            reference: "docker.io/library/alpine:3.20".into(),
        }],
        ..hl_extension::ImageGrant::default()
    });
    let spec = ContainerCreateSpec {
        image: "docker.io/library/alpine:3.20".into(),
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
    let mut unscoped = session(
        &[
            Capability::ContainerCreate,
            Capability::VolumeRead,
            Capability::NetworkWrite,
        ],
        &[],
    )
    .with_volumes(hl_extension::VolumeGrant {
        selectors: vec![],
        create: true,
    })
    .with_networks(hl_extension::NetworkGrant {
        selectors: vec![],
        create: true,
    });
    assert!(matches!(
        unscoped.dispatch(&Request::ContainerCreate { spec: spec.clone() }, &services(&host)),
        Err(Failure::Denied { .. })
    ));
    assert!(host.ledger.reached().is_empty());
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

    let mut insufficient = session(&[Capability::ContainerCreate], &[]);
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
    let mut session = session(&[Capability::ContainerExecute], &[]);
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
    let mut session = session(&[Capability::ContainerExecute], &[]);
    for id in ["friendly".to_owned(), "e1".to_owned(), "a".repeat(12)] {
        assert!(matches!(
            session.dispatch(&Request::ExecutionRemove { id }, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());

    let id = "e".repeat(32);
    session
        .dispatch(
            &Request::ContainerExec {
                id: "c".repeat(64),
                generation: 4,
                command: vec!["true".into()],
                environment: Vec::new(),
                user: None,
                working_directory: None,
                stdin: false,
            },
            &services(&host),
        )
        .unwrap();
    assert_eq!(
        session.dispatch(&Request::ExecutionRemove { id }, &services(&host)),
        Ok(Reply::Done)
    );
    assert_eq!(
        host.ledger.reached(),
        [
            "containers.list",
            "containers.exec",
            "executions.inspect",
            "containers.list",
            "executions.remove"
        ]
    );
}

#[test]
fn execution_control_cannot_be_borrowed_from_readable_inventory_but_survives_reconnect() {
    let host = Host::new();
    let id = "e".repeat(32);

    for mut session in [session(&[Capability::ContainerRead, Capability::ContainerExecute], &[])] {
        assert_eq!(
            session.dispatch(&Request::ExecutionRemove { id: id.clone() }, &services(&host),),
            Err(Failure::Denied {
                capability: "containers:execute".into(),
                detail: "execution control is limited to processes created by this extension incarnation".into(),
            })
        );
    }
    assert!(
        host.ledger.reached().is_empty(),
        "ownership is checked before inventory so a foreign ID leaks no existence"
    );

    let ownership = hl_extension::ExecutionOwnership::default();
    let mut first = session(&[Capability::ContainerExecute], &[]).with_execution_ownership(ownership.clone());
    first
        .dispatch(
            &Request::ContainerExec {
                id: "c".repeat(64),
                generation: 4,
                command: vec!["true".into()],
                environment: Vec::new(),
                user: None,
                working_directory: None,
                stdin: false,
            },
            &services(&host),
        )
        .unwrap();
    drop(first);
    let mut reconnected = session(&[Capability::ContainerExecute], &[]).with_execution_ownership(ownership);
    assert_eq!(
        reconnected.dispatch(&Request::ExecutionRemove { id }, &services(&host)),
        Ok(Reply::Done)
    );
}

#[test]
fn lifecycle_controls_refuse_snapshot_pids_names_and_prefixes_before_control_authority() {
    let host = Host::new();
    let mut session = session(
        &[
            Capability::ContainerLifecycle,
            Capability::ContainerRemove,
            Capability::ContainerExecute,
        ],
        &[],
    );
    for request in [
        Request::ContainerStart {
            id: "friendly-name".into(),
            generation: 4,
        },
        Request::ContainerPause {
            id: "a".repeat(12),
            generation: 4,
        },
        Request::ContainerUnpause {
            id: "friendly-name".into(),
            generation: 4,
        },
        Request::ContainerRestart {
            id: "1".into(),
            generation: 4,
        },
        Request::ContainerStop {
            id: "friendly-name".into(),
            generation: 4,
        },
        Request::ContainerRemove {
            id: "a".repeat(12),
            generation: 4,
        },
        Request::ContainerKill {
            id: "1".into(),
            generation: 4,
            signal: "SIGTERM".into(),
        },
        Request::ContainerKill {
            id: "friendly-name".into(),
            generation: 4,
            signal: "SIGTERM".into(),
        },
        Request::ContainerKill {
            id: "a".repeat(12),
            generation: 4,
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
        let noun = if matches!(request, Request::ExecutionKill { .. }) {
            "execution"
        } else {
            "container"
        };
        let failure = session
            .dispatch(&request, &services(&host))
            .expect_err("mutable identity refused");
        assert_eq!(
            failure,
            Failure::Conflict {
                detail: format!("{noun} operation requires the complete immutable ID returned by inspection"),
            }
        );
    }
    assert!(host.ledger.reached().is_empty());

    session
        .dispatch(
            &Request::ContainerStop {
                id: "a".repeat(64),
                generation: 4,
            },
            &services(&host),
        )
        .unwrap();
    session
        .dispatch(
            &Request::ContainerRemove {
                id: "a".repeat(64),
                generation: 4,
            },
            &services(&host),
        )
        .unwrap();
    session
        .dispatch(
            &Request::ContainerKill {
                id: "a".repeat(64),
                generation: 4,
                signal: "SIGTERM".into(),
            },
            &services(&host),
        )
        .unwrap();
    session
        .dispatch(
            &Request::ContainerExec {
                id: "c".repeat(64),
                generation: 4,
                command: vec!["true".into()],
                environment: Vec::new(),
                user: None,
                working_directory: None,
                stdin: false,
            },
            &services(&host),
        )
        .unwrap();
    session
        .dispatch(
            &Request::ExecutionKill {
                id: "e".repeat(32),
                signal: "SIGTERM".into(),
            },
            &services(&host),
        )
        .unwrap();
    assert_eq!(
        host.ledger.reached(),
        [
            "containers.list",
            "containers.stop",
            "containers.list",
            "containers.remove",
            "containers.list",
            "containers.kill",
            "containers.list",
            "containers.exec",
            "executions.inspect",
            "containers.list",
            "executions.kill"
        ]
    );
}

#[test]
fn container_rename_requires_immutable_identity_and_native_name_grammar() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerLifecycle], &[]);
    for request in [
        Request::ContainerRename {
            id: "friendly-name".into(),
            generation: 4,
            name: "worker".into(),
        },
        Request::ContainerRename {
            id: "a".repeat(12),
            generation: 4,
            name: "worker".into(),
        },
        Request::ContainerRename {
            id: "a".repeat(64),
            generation: 4,
            name: ".worker".into(),
        },
        Request::ContainerRename {
            id: "a".repeat(64),
            generation: 4,
            name: "worker/name".into(),
        },
        Request::ContainerRename {
            id: "a".repeat(64),
            generation: 4,
            name: "x".repeat(129),
        },
    ] {
        let failure = session
            .dispatch(&request, &services(&host))
            .expect_err("invalid rename refused");
        assert!(
            matches!(failure, Failure::Conflict { .. }),
            "unexpected failure: {failure:?}"
        );
    }
    assert!(host.ledger.reached().is_empty());
    session
        .dispatch(
            &Request::ContainerRename {
                id: "a".repeat(64),
                generation: 4,
                name: "worker_2.prod".into(),
            },
            &services(&host),
        )
        .unwrap();
    assert_eq!(host.ledger.reached(), ["containers.list", "containers.rename"]);
}

#[test]
fn image_removal_refuses_mutable_tags_and_partial_digests_before_control_authority() {
    let host = Host::new();
    let digest = format!("sha256:{}", "a".repeat(64));
    let mut session = session(&[Capability::ImageRemove], &[]).with_images(hl_extension::ImageGrant {
        remove: vec![hl_extension::ImageSelector::Digest { digest: digest.clone() }],
        ..hl_extension::ImageGrant::default()
    });
    for reference in ["alpine:latest".to_owned(), "sha256:abc".to_owned(), "a".repeat(64)] {
        assert!(matches!(
            session.dispatch(&Request::ImageRemove { reference }, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
    session
        .dispatch(&Request::ImageRemove { reference: digest }, &services(&host))
        .unwrap();
    assert_eq!(host.ledger.reached(), ["images.remove"]);
}

#[test]
fn image_inventory_filters_every_alias_before_applying_the_bound() {
    let permitted = "docker.io/library/alpine:3.20".to_owned();
    let session = session(&[Capability::ImageRead], &[]).with_images(hl_extension::ImageGrant {
        read: vec![hl_extension::ImageSelector::Reference {
            reference: permitted.clone(),
        }],
        ..hl_extension::ImageGrant::default()
    });
    let inventory = session.visible_images(vec![ImageSummary {
        id: format!("sha256:{}", "a".repeat(64)),
        reference: "registry.example/private:latest".into(),
        references: vec!["registry.example/private:latest".into(), permitted.clone()],
        size: 1,
        created: 0,
    }]);
    assert_eq!(inventory.images.len(), 1);
    assert_eq!(inventory.images[0].reference, permitted);
    assert_eq!(inventory.images[0].references.len(), 1);
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
        [
            "networks.inspect",
            "networks.remove",
            "networks.inspect",
            "containers.list",
            "networks.connect",
            "networks.inspect",
            "containers.list",
            "networks.disconnect"
        ]
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
    assert_eq!(
        host.ledger.reached(),
        ["networks.inspect", "containers.list", "networks.connect"]
    );
}

#[test]
fn volume_removal_requires_the_exact_observed_generation() {
    let host = Host::new();
    let mut session = session(&[Capability::VolumeWrite], &[]);
    assert!(matches!(
        session.dispatch(
            &Request::VolumeRemove {
                name: "cache".into(),
                generation: "legacy-or-stale".into()
            },
            &services(&host),
        ),
        Err(Failure::Conflict { .. })
    ));
    assert!(host.ledger.reached().is_empty());
    session
        .dispatch(
            &Request::VolumeRemove {
                name: "cache".into(),
                generation: "a".repeat(32),
            },
            &services(&host),
        )
        .unwrap();
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
        Failure::Denied { capability, .. } => assert_eq!(capability, "containers:read"),
        other => panic!("an empty list would be worse than a refusal, got {other:?}"),
    }
}

#[test]
fn container_execution_lifecycle_and_removal_authority_are_independent() {
    let host = Host::new();
    let id = "c".repeat(64);
    let exec = Request::ContainerExec {
        id: id.clone(),
        generation: 4,
        command: vec!["psql".into()],
        environment: Vec::new(),
        user: None,
        working_directory: None,
        stdin: false,
    };
    let stop = Request::ContainerStop {
        id: id.clone(),
        generation: 4,
    };
    let remove = Request::ContainerRemove { id, generation: 4 };

    let mut execute_only = session(&[Capability::ContainerExecute], &[]);
    assert!(matches!(
        execute_only.dispatch(&stop, &services(&host)),
        Err(Failure::Denied { ref capability, .. }) if capability == "containers:lifecycle"
    ));
    assert!(matches!(
        execute_only.dispatch(&remove, &services(&host)),
        Err(Failure::Denied { ref capability, .. }) if capability == "containers:remove"
    ));

    let mut lifecycle_only = session(&[Capability::ContainerLifecycle], &[]);
    assert!(matches!(
        lifecycle_only.dispatch(&exec, &services(&host)),
        Err(Failure::Denied { ref capability, .. }) if capability == "containers:execute"
    ));
    assert!(matches!(
        lifecycle_only.dispatch(&remove, &services(&host)),
        Err(Failure::Denied { ref capability, .. }) if capability == "containers:remove"
    ));

    let mut removal_only = session(&[Capability::ContainerRemove], &[]);
    assert!(matches!(
        removal_only.dispatch(&stop, &services(&host)),
        Err(Failure::Denied { ref capability, .. }) if capability == "containers:lifecycle"
    ));
    assert!(matches!(
        removal_only.dispatch(&exec, &services(&host)),
        Err(Failure::Denied { ref capability, .. }) if capability == "containers:execute"
    ));
    assert!(host.ledger.reached().is_empty(), "denied verbs never reach a host port");
}

#[test]
fn container_capabilities_without_resource_consent_expose_nothing() {
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("scoped").unwrap(),
        Grant::new([Capability::ContainerRead, Capability::ContainerLifecycle]),
        Vec::new(),
    ));

    assert_eq!(
        session.dispatch(&Request::ContainerList, &services(&host)).unwrap(),
        Reply::Containers(Vec::new())
    );
    let failure = session
        .dispatch(
            &Request::ContainerStop {
                id: "a".repeat(64),
                generation: 4,
            },
            &services(&host),
        )
        .expect_err("an unselected container is denied");
    assert!(matches!(failure, Failure::Denied { .. }));
    assert!(!host.ledger.reached().contains(&"containers.stop"));
}

#[test]
fn exact_name_scope_filters_inventory_and_create_is_independent() {
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("scoped").unwrap(),
        Grant::new([Capability::ContainerRead, Capability::ContainerCreate]),
        Vec::new(),
    ))
    .with_containers(hl_extension::ContainerGrant {
        selectors: vec![hl_extension::ContainerSelector::Name { name: "api".into() }],
        create: false,
    });

    assert!(matches!(
        session.dispatch(&Request::ContainerList, &services(&host)).unwrap(),
        Reply::Containers(containers) if containers == vec![Host::container()]
    ));
    let creation = Request::ContainerCreate {
        spec: hl_extension::port::ContainerCreateSpec {
            image: "alpine:3.20".into(),
            name: "worker".into(),
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
    };
    assert!(matches!(
        session.dispatch(&creation, &services(&host)),
        Err(Failure::Denied { .. })
    ));
    assert!(!host.ledger.reached().contains(&"containers.create"));
    assert!(matches!(
        session.dispatch(
            &Request::ContainerStop {
                id: "c".repeat(64),
                generation: 4,
            },
            &services(&host),
        ),
        Err(Failure::Denied { .. })
    ));
    assert!(!host.ledger.reached().contains(&"containers.stop"));
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

    assert!(
        session
            .dispatch(
                &Request::FilesystemWrite {
                    path: path("logs/app.log"),
                    contents: b"x".to_vec()
                },
                &services(&host)
            )
            .is_err()
    );
    assert!(
        session
            .dispatch(
                &Request::ContainerStop {
                    id: "c1".into(),
                    generation: 4,
                },
                &services(&host)
            )
            .is_err()
    );
    assert!(
        session
            .dispatch(
                &Request::ContainerKill {
                    id: "c1".into(),
                    generation: 4,
                    signal: "SIGKILL".into(),
                },
                &services(&host),
            )
            .is_err()
    );
    assert!(
        session
            .dispatch(
                &Request::ContainerExec {
                    environment: Vec::new(),
                    id: "c1".into(),
                    generation: 4,
                    command: vec!["sh".into()],
                    user: None,
                    working_directory: None,
                    stdin: false,
                },
                &services(&host),
            )
            .is_err()
    );
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn filesystem_read_and_write_scopes_are_independent_and_fail_before_the_service() {
    let host = Host::new();
    let all = vec![path("src"), path("workspace.toml")];
    let mut session = Session::new(Authority::new(
        ExtensionName::new("sample").unwrap(),
        Grant::new([Capability::FilesystemRead, Capability::FilesystemWrite]),
        all,
    ))
    .with_filesystem(hl_extension::FilesystemGrant {
        read: vec![hl_extension::FilesystemSelector::Subtree { subtree: path("src") }],
        write: vec![hl_extension::FilesystemSelector::Exact {
            exact: path("workspace.toml"),
        }],
        ..hl_extension::FilesystemGrant::default()
    });

    assert!(matches!(
        session.dispatch(&Request::FilesystemInventory, &services(&host)),
        Ok(Reply::FileInventory(FileInventory { entries, complete: true, .. }))
            if entries[0].path.as_str() == "src"
    ));
    assert_eq!(host.ledger.reached(), ["files.inventory"]);
    host.ledger.clear();

    assert!(
        session
            .dispatch(
                &Request::FilesystemRead {
                    path: path("src/lib.rs")
                },
                &services(&host)
            )
            .is_ok()
    );
    assert!(
        session
            .dispatch(
                &Request::FilesystemWrite {
                    path: path("workspace.toml"),
                    contents: b"x".to_vec()
                },
                &services(&host)
            )
            .is_ok()
    );
    host.ledger.clear();
    assert!(matches!(
        session.dispatch(
            &Request::FilesystemWrite {
                path: path("src/lib.rs"),
                contents: b"x".to_vec()
            },
            &services(&host)
        ),
        Err(Failure::Denied { .. })
    ));
    assert!(matches!(
        session.dispatch(
            &Request::FilesystemRead {
                path: path("workspace.toml")
            },
            &services(&host)
        ),
        Err(Failure::Denied { .. })
    ));
    assert!(
        host.ledger.reached().is_empty(),
        "wrong-verb roots must fail before the filesystem port"
    );
}

#[test]
fn filesystem_range_batch_confines_every_member_before_any_host_read() {
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("indexer").unwrap(),
        Grant::new([Capability::FilesystemRead]),
        vec![path("src")],
    ))
    .with_filesystem(hl_extension::FilesystemGrant {
        read: vec![hl_extension::FilesystemSelector::Subtree { subtree: path("src") }],
        ..hl_extension::FilesystemGrant::default()
    });
    let request = Request::FilesystemReadRanges {
        ranges: vec![
            FileRangeRequest {
                path: path("src/lib.rs"),
                offset: 0,
                limit: 8,
                observed: None,
            },
            FileRangeRequest {
                path: path("secrets.env"),
                offset: 0,
                limit: 8,
                observed: None,
            },
        ],
    };
    assert!(matches!(
        session.dispatch(&request, &services(&host)),
        Err(Failure::Denied { .. })
    ));
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn one_file_write_consent_does_not_authorize_create_delete_or_rename() {
    let host = Host::new();
    let file = path("settings.json");
    let mut session = Session::new(Authority::new(
        ExtensionName::new("sample").unwrap(),
        Grant::new([Capability::FilesystemWrite]),
        vec![file.clone()],
    ))
    .with_filesystem(hl_extension::FilesystemGrant {
        write: vec![hl_extension::FilesystemSelector::Exact { exact: file.clone() }],
        ..hl_extension::FilesystemGrant::default()
    });

    assert!(
        session
            .dispatch(
                &Request::FilesystemWrite {
                    path: file.clone(),
                    contents: b"{}".to_vec(),
                },
                &services(&host),
            )
            .is_ok()
    );
    host.ledger.clear();

    for request in [
        Request::FilesystemWrite {
            path: path("sibling.json"),
            contents: b"{}".to_vec(),
        },
        Request::FilesystemWrite {
            path: path("settings.json/child"),
            contents: b"{}".to_vec(),
        },
        Request::FilesystemCreateObserved {
            path: file.clone(),
            contents: b"{}".to_vec(),
        },
        Request::FilesystemRemove { path: file.clone() },
        Request::FilesystemRename {
            from: file.clone(),
            to: path("settings.old.json"),
        },
    ] {
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Denied { .. })
        ));
    }
    assert!(
        host.ledger.reached().is_empty(),
        "unconsented mutation verbs must fail before the filesystem port"
    );
}

#[test]
fn deep_container_reads_return_typed_processes_logs_and_execution_state() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);

    let processes = session
        .dispatch(
            &Request::ContainerProcesses {
                id: "c1".into(),
                snapshot: None,
                after: 0,
                limit: 128,
            },
            &services(&host),
        )
        .expect("process table");
    assert!(matches!(processes, Reply::Processes(table)
        if table.titles == ["PID", "CMD"] && table.observed_at_ms == 1_700_000_000_000
            && table.scope == hl_extension::port::ProcessScope::Namespace
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

    let page = session
        .dispatch(
            &Request::ExecutionOutput {
                id: "e".repeat(32),
                after: 41,
                limit: 16,
            },
            &services(&host),
        )
        .expect("paged execution output");
    assert!(matches!(page, Reply::ExecutionOutput(page)
        if page.next == 42 && !page.eof && !page.gap && page.entries[0].bytes == b"row\n"));

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
fn process_paging_rejects_malformed_snapshots_and_unbounded_limits_before_the_port() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    for request in [
        Request::ContainerProcesses {
            id: "c1".into(),
            snapshot: Some("not-a-snapshot".into()),
            after: 1,
            limit: 1,
        },
        Request::ContainerProcesses {
            id: "c1".into(),
            snapshot: None,
            after: 0,
            limit: 129,
        },
        Request::ContainerProcesses {
            id: "c1".into(),
            snapshot: None,
            after: 1,
            limit: 1,
        },
    ] {
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn execution_wait_rejects_unbounded_timeout_before_calling_host() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    assert!(
        session
            .dispatch(
                &Request::ExecutionWait {
                    id: "e".repeat(32),
                    timeout_ms: 30_001
                },
                &services(&host)
            )
            .is_err()
    );
    assert!(!host.ledger.reached().contains(&"executions.wait"));
}

#[test]
fn execution_cancel_rejects_unbounded_timeout_before_calling_host() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerExecute], &["c1"]);
    assert!(matches!(
        session.dispatch(
            &Request::ExecutionCancel {
                id: "e".repeat(32),
                signal: "SIGTERM".into(),
                timeout_ms: 30_001,
            },
            &services(&host),
        ),
        Err(Failure::Conflict { .. })
    ));
    assert!(!host.ledger.reached().contains(&"executions.cancel"));
}

#[test]
fn execution_reads_refuse_names_and_prefixes_before_inventory_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    for request in [
        Request::ExecutionInspect { id: "worker".into() },
        Request::ExecutionLogs {
            id: "a".repeat(12),
            stdout: true,
            stderr: false,
        },
        Request::ExecutionWait {
            id: "7".into(),
            timeout_ms: 500,
        },
    ] {
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn execution_logs_require_a_stream_before_calling_host() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    assert!(
        session
            .dispatch(
                &Request::ExecutionLogs {
                    id: "e".repeat(32),
                    stdout: false,
                    stderr: false
                },
                &services(&host)
            )
            .is_err()
    );
    assert!(!host.ledger.reached().contains(&"executions.logs"));
}

#[test]
fn notifications_are_bounded_and_denied_before_host_delivery() {
    let host = Host::new();
    let request = Request::NotificationPublish {
        notification: hl_extension::Notification {
            id: "build".into(),
            title: "Index ready".into(),
            body: "Background indexing completed".into(),
        },
    };
    assert!(session(&[], &[]).dispatch(&request, &services(&host)).is_err());
    assert!(host.ledger.reached().is_empty());
    let mut allowed = session(&[Capability::NotificationPublish], &[]);
    assert_eq!(allowed.dispatch(&request, &services(&host)), Ok(Reply::Done));
    let invalid = Request::NotificationPublish {
        notification: hl_extension::Notification {
            id: "bad\nidentity".into(),
            title: "Index ready".into(),
            body: "done".into(),
        },
    };
    assert!(allowed.dispatch(&invalid, &services(&host)).is_err());
    assert_eq!(host.ledger.reached(), vec!["notifications.publish"]);
}

#[test]
fn notification_failure_is_structured_and_the_session_remains_usable() {
    let host = Host::new();
    host.fail_notification.set(true);
    let mut allowed = session(&[Capability::NotificationPublish, Capability::WorkspaceRead], &[]);
    let request = Request::NotificationPublish {
        notification: hl_extension::Notification {
            id: "monitor".into(),
            title: "Database".into(),
            body: "offline".into(),
        },
    };
    assert!(matches!(
        allowed.dispatch(&request, &services(&host)),
        Err(Failure::Failed { .. })
    ));
    assert!(matches!(
        allowed.dispatch(&Request::WorkspaceInfo, &services(&host)),
        Ok(Reply::Workspace(_))
    ));
}

#[test]
fn one_session_cannot_multiply_unbounded_notification_identities() {
    let host = Host::new();
    let mut allowed = session(&[Capability::NotificationPublish], &[]);
    for index in 0..32 {
        let request = Request::NotificationPublish {
            notification: hl_extension::Notification {
                id: format!("job-{index}"),
                title: "Job".into(),
                body: "done".into(),
            },
        };
        assert_eq!(allowed.dispatch(&request, &services(&host)), Ok(Reply::Done));
    }
    let overflow = Request::NotificationPublish {
        notification: hl_extension::Notification {
            id: "job-32".into(),
            title: "Job".into(),
            body: "done".into(),
        },
    };
    assert!(matches!(
        allowed.dispatch(&overflow, &services(&host)),
        Err(Failure::Conflict { .. })
    ));
}

#[test]
fn execution_output_window_is_bounded_before_inventory_authority() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    for limit in [0, 17] {
        assert!(matches!(
            session.dispatch(
                &Request::ExecutionOutput {
                    id: "e".repeat(32),
                    after: 0,
                    limit,
                },
                &services(&host),
            ),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn execution_output_requires_the_executions_container_scope_before_retrieval() {
    let host = Host::new();
    *host.execution_container.borrow_mut() = "c2".into();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("sample").expect("name"),
        Grant::new([Capability::ContainerRead]),
        Vec::new(),
    ))
    .with_containers(hl_extension::ContainerGrant {
        selectors: vec![hl_extension::ContainerSelector::Name { name: "api".into() }],
        create: false,
    });

    assert!(matches!(
        session.dispatch(
            &Request::ExecutionOutput {
                id: "e".repeat(32),
                after: 0,
                limit: 16,
            },
            &services(&host),
        ),
        Err(Failure::Denied { capability, .. }) if capability == "containers:read"
    ));
    assert_eq!(
        host.ledger.reached(),
        vec!["executions.inspect", "containers.list"],
        "hidden execution output must not reach its output adapter"
    );
}

#[test]
fn execution_output_still_pages_for_an_execution_in_scope() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);
    let reply = session
        .dispatch(
            &Request::ExecutionOutput {
                id: "e".repeat(32),
                after: 41,
                limit: 1,
            },
            &services(&host),
        )
        .expect("in-scope output");

    assert!(matches!(reply, Reply::ExecutionOutput(page)
        if page.next == 42 && page.entries.len() == 1 && page.entries[0].bytes == b"row\n"));
    assert_eq!(
        host.ledger.reached(),
        vec!["executions.inspect", "containers.list", "executions.output"]
    );
}

#[test]
fn execution_stdin_is_bounded_authorized_and_explicitly_half_closed() {
    let host = Host::new();
    let id = "e".repeat(32);
    let write = Request::ExecutionWrite {
        id: id.clone(),
        contents: b"select 1;\n".to_vec(),
    };
    let mut denied = session(&[Capability::ContainerRead], &[]);
    assert!(matches!(
        denied.dispatch(&write, &services(&host)),
        Err(Failure::Denied { capability, .. }) if capability == "containers:input"
    ));
    assert!(host.ledger.reached().is_empty());

    let mut foreign = session(&[Capability::ContainerInput], &[]);
    assert!(matches!(
        foreign.dispatch(&write, &services(&host)),
        Err(Failure::Denied { detail, .. })
            if detail == "execution control is limited to processes created by this extension incarnation"
    ));
    assert!(host.ledger.reached().is_empty(), "foreign input must be rejected before lookup");

    let ownership = hl_extension::ExecutionOwnership::default();
    ownership.lock().expect("ownership").insert(id.clone());
    let mut allowed = session(&[Capability::ContainerInput], &[]).with_execution_ownership(ownership);
    for contents in [Vec::new(), vec![0; 65_537]] {
        assert!(matches!(
            allowed.dispatch(
                &Request::ExecutionWrite {
                    id: id.clone(),
                    contents,
                },
                &services(&host),
            ),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty(), "invalid chunks reached the host");

    assert_eq!(allowed.dispatch(&write, &services(&host)), Ok(Reply::Done));
    assert_eq!(
        allowed.dispatch(&Request::ExecutionCloseInput { id }, &services(&host),),
        Ok(Reply::Done)
    );
    assert_eq!(host.execution_input.borrow().as_slice(), [b"select 1;\n".to_vec()]);
    assert_eq!(
        host.ledger.reached(),
        vec![
            "executions.inspect",
            "containers.list",
            "executions.write",
            "executions.inspect",
            "containers.list",
            "executions.close_input",
        ]
    );
}

#[test]
fn container_exec_returns_the_real_execution_identity() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerExecute], &[]);
    let immutable = "c".repeat(64);
    let refused = session.dispatch(
        &Request::ContainerExec {
            environment: Vec::new(),
            id: "worker".into(),
            generation: 4,
            command: vec!["worker".into()],
            user: None,
            working_directory: None,
            stdin: false,
        },
        &services(&host),
    );
    assert_eq!(
        refused,
        Err(Failure::Conflict {
            detail: "container operation requires the complete immutable ID returned by inspection".into(),
        })
    );
    assert!(
        host.ledger.reached().is_empty(),
        "a mutable alias reached execution authority"
    );
    let reply = session
        .dispatch(
            &Request::ContainerExec {
                environment: Vec::new(),
                id: immutable,
                generation: 4,
                command: vec!["worker".into()],
                user: Some("1000".into()),
                working_directory: Some("/work".into()),
                stdin: false,
            },
            &services(&host),
        )
        .expect("exec starts");
    assert_eq!(reply, Reply::Identity("e".repeat(32)));
    assert_eq!(host.ledger.reached(), vec!["containers.list", "containers.exec"]);
}

#[test]
fn retaining_execution_stdin_requires_input_authority_in_addition_to_execute() {
    let host = Host::new();
    let request = Request::ContainerExec {
        id: "c".repeat(64),
        generation: 4,
        command: vec!["cat".into()],
        environment: Vec::new(),
        user: None,
        working_directory: None,
        stdin: true,
    };
    let mut execute_only = session(&[Capability::ContainerExecute], &[]);
    assert!(matches!(
        execute_only.dispatch(&request, &services(&host)),
        Err(Failure::Denied { capability, .. }) if capability == "containers:input"
    ));
    assert!(host.ledger.reached().is_empty());

    let mut interactive = session(&[Capability::ContainerExecute, Capability::ContainerInput], &[]);
    assert_eq!(
        interactive.dispatch(&request, &services(&host)),
        Ok(Reply::Identity("e".repeat(32)))
    );
}

#[test]
fn exec_environment_is_bounded_unique_and_redacted_before_service_access() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerExecute], &[]);
    let id = "c".repeat(64);
    let secret = "sentinel-password-never-observable";
    let request = Request::ContainerExec {
        id,
        generation: 4,
        command: vec!["psql".into()],
        environment: vec![
            ("PGPASSWORD".into(), hl_extension::ExecEnvironmentValue::new(secret)),
            (
                "PGPASSWORD".into(),
                hl_extension::ExecEnvironmentValue::new("duplicate"),
            ),
        ],
        user: None,
        working_directory: None,
        stdin: false,
    };
    assert!(!format!("{request:?}").contains(secret));
    let failure = session
        .dispatch(&request, &services(&host))
        .expect_err("duplicate refused");
    assert!(matches!(failure, Failure::Conflict { .. }));
    assert!(!format!("{failure:?}").contains(secret));
    assert!(
        host.ledger.reached().is_empty(),
        "invalid environment reached container inventory or control"
    );

    for environment in [
        vec![("BAD=NAME".into(), hl_extension::ExecEnvironmentValue::new("x"))],
        vec![(
            "PGPASSWORD".into(),
            hl_extension::ExecEnvironmentValue::new("x".repeat(8193)),
        )],
        (0..9)
            .map(|index| {
                (
                    format!("V{index}"),
                    hl_extension::ExecEnvironmentValue::new("x".repeat(8192)),
                )
            })
            .collect(),
    ] {
        let request = Request::ContainerExec {
            id: "c".repeat(64),
            generation: 4,
            command: vec!["psql".into()],
            environment,
            user: None,
            working_directory: None,
            stdin: false,
        };
        assert!(matches!(
            session.dispatch(&request, &services(&host)),
            Err(Failure::Conflict { .. })
        ));
    }
    assert!(host.ledger.reached().is_empty());

    let reply = session
        .dispatch(
            &Request::ContainerExec {
                id: "c".repeat(64),
                generation: 4,
                command: vec!["psql".into()],
                environment: vec![("PGPASSWORD".into(), hl_extension::ExecEnvironmentValue::new(secret))],
                user: None,
                working_directory: None,
                stdin: false,
            },
            &services(&host),
        )
        .expect("valid environment reaches exec");
    assert_eq!(reply, Reply::Identity("e".repeat(32)));
    assert!(!format!("{reply:?}").contains(secret));
}

#[test]
fn credential_execution_requires_both_grants_and_resolves_only_inside_the_host() {
    let request = Request::ContainerExecCredential {
        id: "a".repeat(64),
        generation: 0,
        command: vec!["psql".into()],
        environment: Vec::new(),
        credentials: vec![("PGPASSWORD".into(), "postgres.password".into())],
        user: None,
        working_directory: None,
        stdin: false,
    };
    let host = Host::new();
    let state = CredentialPort { read: Cell::new(false) };
    let mut missing = session(&[Capability::ContainerExecute], &[]);
    assert!(
        matches!(missing.dispatch(&request, &services_with_state(&host, &state)),
        Err(Failure::Denied { capability, .. }) if capability == "credentials:inject")
    );
    assert!(!state.read.get());
    assert!(host.ledger.reached().is_empty());

    let mut granted = session(&[Capability::ContainerExecute, Capability::CredentialInject], &[]);
    assert!(
        matches!(granted.dispatch(&request, &services_with_state(&host, &state)), Ok(Reply::Identity(id)) if id == "e".repeat(32))
    );
    assert!(state.read.get());
    assert_eq!(host.ledger.reached(), vec!["containers.list", "containers.exec"]);
}

#[test]
fn credential_read_rejects_a_host_value_for_another_key() {
    let host = Host::new();
    let mut client = session(&[Capability::CredentialRead], &[]);
    let result = client.dispatch(
        &Request::CredentialRead {
            key: "postgres.password".into(),
        },
        &services_with_state(&host, &MismatchedCredentialPort),
    );
    assert!(matches!(result, Err(Failure::Failed { detail }) if detail.contains("another key")));
}

#[test]
fn volume_and_network_reads_and_safe_controls_use_distinct_grants() {
    let host = Host::new();
    let mut read = session(&[Capability::VolumeRead, Capability::NetworkRead], &[]);
    assert!(
        matches!(read.dispatch(&Request::VolumeList, &services(&host)), Ok(Reply::Volumes(values)) if values.volumes[0].name == "cache")
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
fn exact_network_scope_filters_inventory_and_denies_unrelated_inspection_and_creation() {
    let host = Host::new();
    let mut scoped =
        session(&[Capability::NetworkRead, Capability::NetworkWrite], &[]).with_networks(hl_extension::NetworkGrant {
            selectors: vec![hl_extension::NetworkSelector::Name { name: "private".into() }],
            create: false,
        });
    let Reply::Networks(inventory) = scoped.dispatch(&Request::NetworkList, &services(&host)).unwrap() else {
        panic!("network inventory reply");
    };
    assert_eq!(
        inventory
            .networks
            .iter()
            .map(|network| network.name.as_str())
            .collect::<Vec<_>>(),
        ["private"]
    );
    host.ledger.clear();
    assert!(matches!(
        scoped.dispatch(
            &Request::NetworkInspect {
                reference: "unrelated".into()
            },
            &services(&host)
        ),
        Err(Failure::Denied { .. })
    ));
    assert!(matches!(
        scoped.dispatch(&Request::NetworkCreate { name: "another".into() }, &services(&host)),
        Err(Failure::Denied { .. })
    ));
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn exact_volume_scope_filters_and_denies_before_host_access() {
    let host = Host::new();
    let mut scoped =
        session(&[Capability::VolumeRead, Capability::VolumeWrite], &[]).with_volumes(hl_extension::VolumeGrant {
            selectors: vec![hl_extension::VolumeSelector::Name { name: "cache".into() }],
            create: false,
        });
    assert!(
        matches!(scoped.dispatch(&Request::VolumeList, &services(&host)), Ok(Reply::Volumes(values)) if values.volumes.len() == 1)
    );
    host.ledger.clear();
    assert!(matches!(
        scoped.dispatch(&Request::VolumeInspect { name: "other".into() }, &services(&host)),
        Err(Failure::Denied { .. })
    ));
    assert!(matches!(
        scoped.dispatch(
            &Request::VolumeInspect {
                name: "bad/name".into()
            },
            &services(&host)
        ),
        Err(Failure::Conflict { .. })
    ));
    assert!(host.ledger.reached().is_empty());
}

#[test]
fn volume_creation_requires_create_and_selection_to_prevent_namesake_capture() {
    let host = Host::new();
    let mut create_only = session(&[Capability::VolumeWrite], &[]).with_volumes(hl_extension::VolumeGrant {
        selectors: vec![],
        create: true,
    });
    assert!(matches!(
        create_only.dispatch(&Request::VolumeCreate { name: "cache".into() }, &services(&host)),
        Err(Failure::Denied { .. })
    ));
    assert!(host.ledger.reached().is_empty());
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
    assert_eq!(host.ledger.reached(), vec!["containers.list", "containers.inspect"]);
}

#[test]
fn observed_container_inspection_rejects_a_changed_lifecycle_generation() {
    let host = Host::new();
    let mut session = session(&[Capability::ContainerRead], &[]);

    assert!(matches!(
        session.dispatch(
            &Request::ContainerInspectObserved {
                id: "c1".into(),
                generation: 1,
            },
            &services(&host),
        ),
        Err(Failure::Conflict { detail }) if detail.contains("changed from 1 to 0")
    ));
    assert_eq!(host.ledger.reached(), vec!["containers.list", "containers.inspect"]);
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

    assert!(
        session
            .dispatch(&Request::EventSubscribe { topic: Topic::Terminal }, &services(&host))
            .is_err()
    );
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
    let mut denied = session(
        &[Capability::ContainerLifecycle, Capability::TerminalLayoutControl],
        &[],
    );
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
    assert_eq!(
        host.ledger.reached(),
        vec!["containers.list", "terminal.attach_container"]
    );

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
