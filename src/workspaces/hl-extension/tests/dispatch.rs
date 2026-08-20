//! Dispatch and enforcement, driven entirely by in-memory ports.
//!
//! No container runtime, no socket, no toolkit. That this suite runs at all is
//! the evidence that the ports-and-adapters split is real rather than
//! decorative: if the protocol had reached for a service directly, none of this
//! could be written.

use std::cell::RefCell;

use hl_extension::port::{
    ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
    Occupant, PaneSummary, PaneText, TabSummary, TerminalSurface, WorkspaceFiles, WorkspaceInventory, WorkspaceState,
};
use hl_extension::{
    Authority, Capability, ExtensionName, Failure, Grant, RelativePath, Reply, Request, Services, Session, Topic,
    WorkspaceInfo,
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
}

impl Host {
    fn new() -> Self {
        Self {
            ledger: Ledger::default(),
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
}

impl ContainerControl for Host {
    fn create(&self, _image: &str, name: &str) -> Result<String, HostError> {
        self.ledger.note("containers.create");
        Ok(format!("id-{name}"))
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
}

impl TerminalSurface for Host {
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
            }],
        }])
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
        Ok(PaneText {
            slot: slot.into(),
            lines: vec![format!("at most {lines}")],
            truncated: true,
        })
    }

    fn close(&self, _slot: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn focus(&self, _slot: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn ratio(&self, _slot: &str, _ratio: f64) -> Result<(), HostError> {
        Ok(())
    }

    fn surface(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Ok("s3".into())
    }
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

    fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
        self.ledger.note("files.write");
        Ok(())
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
        containers: host,
        control: host,
        images: host,
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

/// Every call, paired with the capability that must permit it.
fn calls() -> Vec<(Request, Capability)> {
    vec![
        (Request::WorkspaceInfo, Capability::WorkspaceRead),
        (Request::ContainerList, Capability::ContainerRead),
        (Request::ContainerInspect { id: "c1".into() }, Capability::ContainerRead),
        (
            Request::ContainerCreate {
                image: "alpine".into(),
                name: "x".into(),
            },
            Capability::ContainerControl,
        ),
        (
            Request::ContainerStart { id: "c1".into() },
            Capability::ContainerControl,
        ),
        (Request::ContainerStop { id: "c1".into() }, Capability::ContainerControl),
        (
            Request::ContainerRemove { id: "c1".into() },
            Capability::ContainerControl,
        ),
        (Request::ImageList, Capability::ImageRead),
        (
            Request::ImagePull {
                reference: "alpine".into(),
            },
            Capability::ImageWrite,
        ),
        (Request::TerminalTabs, Capability::TerminalRead),
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
            Request::FilesystemWrite {
                path: path("logs/app.log"),
                contents: b"x".to_vec(),
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
fn a_session_owns_one_tab_however_often_it_asks() {
    let host = Host::new();
    let mut session = session(&[Capability::Interface], &[]);
    let services = services(&host);
    let request = Request::InterfaceOpenTab {
        title: "Postgres".into(),
    };

    let first = session.dispatch(&request, &services).expect("opened");
    let second = session.dispatch(&request, &services).expect("opened again");

    assert_eq!(first, second);
    assert_eq!(
        host.ledger.reached(),
        vec!["terminal.open_tab"],
        "asking twice must not accumulate surfaces the extension forgot about"
    );
    assert_eq!(session.tab(), Some("tab-Postgres"));
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
    assert_eq!(collected, vec![frame], "the host receives exactly what was sent");
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
