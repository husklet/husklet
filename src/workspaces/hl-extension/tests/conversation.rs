//! A whole extension conversation over a real socket.
//!
//! The two sides here are shaped like two processes: an extension on its own
//! thread that owns one end of a connected [`Stream`] pair and never touches
//! the host's state, and a host on the main thread that owns a [`Session`], in-memory
//! ports, and a real [`hl_gui::Tree`]. Nothing is shared but bytes.
//!
//! What this proves is the claim the whole protocol exists to make: an
//! interface composed out of process arrives whole. The assertions therefore
//! walk the reconstructed tree and compare tag, properties, handlers, and
//! parentage against what the extension described, because a test that only
//! checked for the absence of an error would pass on an empty tree.

use std::cell::{Cell, RefCell};

use hl_extension::port::{
    ContainerControl, ContainerCreateSpec, ContainerInventory, ContainerSummary, ContainerVolumeMount, Division, Entry,
    HostError, ImageStore, ImageSummary, PaneText, TabSummary, TerminalSurface, WorkspaceFiles, WorkspaceInventory,
    WorkspaceState,
};
use hl_extension::{
    codec, Authority, Capability, Coding, ExtensionName, Failure, Grant, Hello, RelativePath, Reply, Request, Services,
    Session, Transit, Welcome, WorkspaceInfo, PROTOCOL,
};
use hl_gui::{
    Align, Choice, Column as TableColumn, EventId, Length, NodeId, Patch, Prop, PropValue, RowWindow, Scale, SourceId,
    Surface, Theme, Tone, Tree, Trigger, Variant,
};

/// The concrete stream this conversation runs over.
///
/// The protocol is defined over `Read + Write`, and what these tests need from a transport is
/// only that the two ends are connected to each other, can be moved to another thread, and close
/// ordinarily. `UnixStream::pair` is that on Unix. `std` binds no Unix-domain socket off Unix, and
/// a loopback TCP connection has the same three properties, so the conversation is exercised on
/// every host instead of vanishing on one -- an integration target that compiles to zero tests
/// reports `ok` and proves nothing.
#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
type Stream = std::net::TcpStream;

/// Two connected [`Stream`] endpoints.
fn connected_pair() -> (Stream, Stream) {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::pair().expect("a socket pair")
    }
    #[cfg(not(unix))]
    {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a loopback listener");
        let ours = std::net::TcpStream::connect(listener.local_addr().expect("a bound address")).expect("connected");
        let (theirs, _) = listener.accept().expect("accepted");
        (ours, theirs)
    }
}

#[test]
fn removed_workspace_adopt_call_is_rejected_and_the_socket_remains_decodable() {
    let (host_end, extension_end) = connected_pair();
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);
    sender
        .send(&hl_rpc::Frame::new(
            codec::CALLS,
            hl_rpc::Kind::Request,
            br#"{"call":"workspace_adopt","with":{"configuration":{}}}"#.to_vec(),
        ))
        .expect("obsolete call sent");
    assert!(
        codec::read_request(&receiver.receive().expect("obsolete frame")).is_err(),
        "the removed call must not decode into host authority"
    );

    sender
        .send(&codec::request(&Request::WorkspaceInfo).expect("current request"))
        .expect("current request sent");
    assert!(matches!(
        codec::read_request(&receiver.receive().expect("current frame")),
        Ok(Request::WorkspaceInfo)
    ));
}

// ---------------------------------------------------------------------------
// The host's in-memory ports.
// ---------------------------------------------------------------------------

/// Host services backed by nothing, so the conversation exercises the protocol
/// rather than a container runtime.
struct Host {
    tabs: RefCell<Vec<String>>,
    network_aliases: RefCell<Vec<String>>,
    filesystem_pages: Cell<usize>,
    filesystem_ranges: Cell<usize>,
    filesystem_file: RefCell<Option<(RelativePath, Vec<u8>, String)>>,
    execution_output_calls: Cell<usize>,
}
impl hl_extension::port::VolumeStore for Host {}
impl hl_extension::port::NetworkStore for Host {
    fn inspect(&self, reference: &str) -> Result<hl_extension::port::NetworkSummary, HostError> {
        Ok(hl_extension::port::NetworkSummary {
            id: "a".repeat(32),
            name: reference.into(),
            driver: "bridge".into(),
            scope: "local".into(),
            kind: hl_extension::NetworkKind::Custom,
            endpoints: None,
        })
    }

    fn connect_with_aliases(&self, _reference: &str, _container: &str, aliases: &[String]) -> Result<(), HostError> {
        *self.network_aliases.borrow_mut() = aliases.to_vec();
        Ok(())
    }
}

impl Host {
    fn new() -> Self {
        Self {
            tabs: RefCell::new(Vec::new()),
            network_aliases: RefCell::new(Vec::new()),
            filesystem_pages: Cell::new(0),
            filesystem_ranges: Cell::new(0),
            filesystem_file: RefCell::new(None),
            execution_output_calls: Cell::new(0),
        }
    }
}

impl ContainerInventory for Host {
    fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
        Ok(Vec::new())
    }

    fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
        Err(HostError::Absent(id.into()))
    }

    fn execution(&self, id: &str) -> Result<hl_extension::port::ExecutionSummary, HostError> {
        Ok(hl_extension::port::ExecutionSummary {
            id: id.into(),
            container_id: "c2".into(),
            running: true,
            exit_code: 0,
            result: None,
            created_at_ms: Some(5),
            started_at_ms: Some(6),
            finished_at_ms: None,
            pid: 9,
            command: vec!["psql".into()],
            user: "postgres".into(),
        })
    }

    fn execution_output(
        &self,
        _id: &str,
        after: u64,
        _limit: u16,
    ) -> Result<hl_extension::port::ExecutionOutputPage, HostError> {
        self.execution_output_calls.set(self.execution_output_calls.get() + 1);
        Ok(hl_extension::port::ExecutionOutputPage {
            entries: vec![hl_extension::port::ExecutionOutputEntry {
                sequence: after + 1,
                timestamp_ms: 1,
                stream: "stdout".into(),
                bytes: b"secret row\n".to_vec(),
            }],
            next: after + 1,
            more: false,
            eof: false,
            gap: false,
        })
    }
}

impl ContainerControl for Host {
    fn create(&self, _image: &str, name: &str) -> Result<String, HostError> {
        Ok(format!("id-{name}"))
    }

    fn start(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        Ok(())
    }

    fn stop(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        Ok(())
    }

    fn remove(&self, _id: &str, _expected_id: &str, _generation: u64) -> Result<(), HostError> {
        Ok(())
    }
}

impl ImageStore for Host {
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        Ok(Vec::new())
    }
}

impl TerminalSurface for Host {
    fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
        Ok(Vec::new())
    }

    fn open_tab(&self, title: &str) -> Result<String, HostError> {
        self.tabs.borrow_mut().push(title.into());
        Ok(format!("tab-{title}"))
    }

    fn split(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Ok("s2".into())
    }

    fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
        Ok(())
    }
    fn read(&self, slot: &str, lines: usize) -> Result<PaneText, HostError> {
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

impl hl_extension::port::WorkspaceControl for Host {}

impl WorkspaceFiles for Host {
    fn list(&self, _path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        Ok(Vec::new())
    }

    fn read(&self, _path: &RelativePath) -> Result<Vec<u8>, HostError> {
        Ok(Vec::new())
    }

    fn read_range(
        &self,
        path: &RelativePath,
        offset: u64,
        limit: usize,
        _observed: Option<&str>,
    ) -> Result<hl_extension::port::FileRange, HostError> {
        self.filesystem_ranges.set(self.filesystem_ranges.get() + 1);
        Ok(hl_extension::port::FileRange {
            path: path.clone(),
            identity: "large-v1".into(),
            offset,
            total: offset + limit as u64,
            contents: vec![b'x'; limit],
            eof: true,
            truncated: false,
        })
    }

    fn list_page(
        &self,
        _path: &RelativePath,
        _after: Option<&RelativePath>,
        _observed: Option<&str>,
        _limit: usize,
    ) -> Result<hl_extension::port::DirectoryPage, HostError> {
        self.filesystem_pages.set(self.filesystem_pages.get() + 1);
        Ok(hl_extension::port::DirectoryPage {
            entries: vec![Entry {
                path: RelativePath::new("src/b.ts").expect("path"),
                directory: false,
                size: 7,
                identity: None,
            }],
            identity: "directory-v1".into(),
            next: Some(RelativePath::new("src/b.ts").expect("path")),
            more: true,
        })
    }

    fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
        Ok(())
    }

    fn read_link(&self, path: &RelativePath) -> Result<Vec<u8>, HostError> {
        if path.as_str() == "src/current" {
            Ok(b"generated/current.ts".to_vec())
        } else {
            Err(HostError::Absent(path.to_string()))
        }
    }

    fn create_observed(&self, path: &RelativePath, contents: &[u8]) -> Result<String, HostError> {
        let mut file = self.filesystem_file.borrow_mut();
        if file.is_some() {
            return Err(HostError::Conflict(format!("{path} already exists")));
        }
        let identity = "file-v1".to_owned();
        *file = Some((path.clone(), contents.to_vec(), identity.clone()));
        Ok(identity)
    }

    fn write_observed(&self, path: &RelativePath, observed: &str, contents: &[u8]) -> Result<String, HostError> {
        let mut file = self.filesystem_file.borrow_mut();
        let Some((held_path, held_contents, identity)) = file.as_mut() else {
            return Err(HostError::Absent(path.to_string()));
        };
        if held_path != path || identity != observed {
            return Err(HostError::Conflict(format!("{path} changed since it was observed")));
        }
        *held_contents = contents.to_vec();
        *identity = "file-v2".to_owned();
        Ok(identity.clone())
    }

    fn remove_observed(&self, path: &RelativePath, observed: &str) -> Result<(), HostError> {
        let mut file = self.filesystem_file.borrow_mut();
        let Some((held_path, _, identity)) = file.as_ref() else {
            return Err(HostError::Absent(path.to_string()));
        };
        if held_path != path || identity != observed {
            return Err(HostError::Conflict(format!("{path} changed since it was observed")));
        }
        *file = None;
        Ok(())
    }
}

#[test]
fn a_scoped_symlink_target_crosses_the_real_socket_without_following_it() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("indexer").expect("name"),
        Grant::new([Capability::FilesystemRead]),
        Vec::new(),
    ))
    .with_filesystem(hl_extension::FilesystemGrant {
        read: vec![hl_extension::FilesystemSelector::Exact {
            exact: RelativePath::new("src/current").expect("read root"),
        }],
        ..hl_extension::FilesystemGrant::default()
    });
    let mut extension = hl_extension::Wire::new(extension_end);
    let mut server = hl_extension::Wire::new(host_end);
    let request = Request::FilesystemReadLink {
        path: RelativePath::new("src/current").expect("path"),
    };

    extension
        .send(&codec::request(&request).expect("request frame"))
        .expect("request sent");
    let decoded = codec::read_request(&server.receive().expect("request crossed socket")).expect("request decoded");
    let reply = session
        .dispatch(&decoded, &services(&host))
        .expect("authorized link read");
    server
        .send(&codec::reply(&reply).expect("reply frame"))
        .expect("reply sent");

    assert_eq!(
        codec::read_reply(&extension.receive().expect("reply crossed socket")).expect("reply decoded"),
        Reply::Contents(b"generated/current.ts".to_vec())
    );
    assert_eq!(
        session.dispatch(
            &Request::FilesystemReadLink {
                path: RelativePath::new("private/key").expect("path"),
            },
            &services(&host),
        ),
        Err(Failure::Denied {
            capability: Capability::FilesystemRead.as_str().into(),
            detail: "path is outside the extension's consented resource scope".into(),
        })
    );
}

#[test]
fn observed_file_mutations_reject_a_stale_agent_and_keep_the_real_socket_usable() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("agent").expect("name"),
        Grant::new([Capability::FilesystemWrite]),
        Vec::new(),
    ))
    .with_filesystem(hl_extension::FilesystemGrant {
        create: vec![hl_extension::FilesystemSelector::Exact {
            exact: RelativePath::new("src/config.ts").expect("create root"),
        }],
        write: vec![hl_extension::FilesystemSelector::Exact {
            exact: RelativePath::new("src/config.ts").expect("write root"),
        }],
        delete: vec![hl_extension::FilesystemSelector::Exact {
            exact: RelativePath::new("src/config.ts").expect("delete root"),
        }],
        ..hl_extension::FilesystemGrant::default()
    });
    let mut extension = hl_extension::Wire::new(extension_end);
    let mut server = hl_extension::Wire::new(host_end);

    let exchange = |request: Request,
                    session: &mut Session,
                    extension: &mut hl_extension::Wire<Stream>,
                    server: &mut hl_extension::Wire<Stream>|
     -> Result<Reply, Failure> {
        extension
            .send(&codec::request(&request).expect("request frame"))
            .expect("sent");
        let decoded = codec::read_request(&server.receive().expect("request crossed socket")).expect("decoded");
        match session.dispatch(&decoded, &services(&host)) {
            Ok(reply) => {
                server
                    .send(&codec::reply(&reply).expect("reply frame"))
                    .expect("reply sent");
                codec::read_reply(&extension.receive().expect("reply crossed socket")).map_err(|error| {
                    Failure::Failed {
                        detail: error.to_string(),
                    }
                })
            }
            Err(failure) => {
                server
                    .send(&codec::failure(&failure).expect("failure frame"))
                    .expect("failure sent");
                Err(
                    codec::read_failure(&extension.receive().expect("failure crossed socket"))
                        .expect("failure decoded"),
                )
            }
        }
    };
    let path = RelativePath::new("src/config.ts").expect("path");

    let created = exchange(
        Request::FilesystemCreateObserved {
            path: path.clone(),
            contents: b"export const port = 5432;".to_vec(),
        },
        &mut session,
        &mut extension,
        &mut server,
    )
    .expect("created");
    assert_eq!(created, Reply::Identity("file-v1".into()));

    let replaced = exchange(
        Request::FilesystemWriteObserved {
            path: path.clone(),
            observed: "file-v1".into(),
            contents: b"export const port = 5433;".to_vec(),
        },
        &mut session,
        &mut extension,
        &mut server,
    )
    .expect("replaced");
    assert_eq!(replaced, Reply::Identity("file-v2".into()));

    assert!(matches!(
        exchange(
            Request::FilesystemRemoveObserved {
                path: path.clone(),
                observed: "file-v1".into(),
            },
            &mut session,
            &mut extension,
            &mut server,
        ),
        Err(Failure::Conflict { .. })
    ));
    assert_eq!(
        host.filesystem_file.borrow().as_ref().expect("file retained").1,
        b"export const port = 5433;",
        "a stale delete cannot destroy the newer edit"
    );

    assert_eq!(
        exchange(
            Request::FilesystemRemoveObserved {
                path,
                observed: "file-v2".into(),
            },
            &mut session,
            &mut extension,
            &mut server,
        )
        .expect("removed after conflict"),
        Reply::Done,
        "the same socket remains framed and usable after a conflict reply"
    );
    assert!(host.filesystem_file.borrow().is_none());
}

#[test]
fn bounded_directory_cursor_crosses_the_real_socket() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("indexer").expect("name"),
        Grant::new([Capability::FilesystemRead]),
        Vec::new(),
    ))
    .with_filesystem(hl_extension::FilesystemGrant {
        read: vec![hl_extension::FilesystemSelector::Subtree {
            subtree: RelativePath::new("src").expect("root"),
        }],
        ..hl_extension::FilesystemGrant::default()
    });
    let request = Request::FilesystemListPage {
        path: RelativePath::new("src").expect("path"),
        after: Some(RelativePath::new("src/a.ts").expect("cursor")),
        observed: Some("directory-v1".into()),
        limit: 1,
    };
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);
    let invalid = Request::FilesystemListPage {
        path: RelativePath::new("src").expect("path"),
        after: Some(RelativePath::new("src/a.ts").expect("cursor")),
        observed: None,
        limit: 1,
    };
    sender
        .send(&codec::request(&invalid).expect("invalid request"))
        .expect("sent");
    let frame = receiver.receive().expect("invalid frame");
    let failure = session
        .dispatch(
            &codec::read_request(&frame).expect("invalid request decodes"),
            &services(&host),
        )
        .expect_err("unbound continuation refused");
    receiver
        .send(&codec::failure(&failure).expect("failure frame"))
        .expect("failure sent");
    assert!(matches!(
        codec::read_failure(&sender.receive().expect("failure reply")),
        Ok(Failure::Failed { .. })
    ));
    assert_eq!(host.filesystem_pages.get(), 0, "bounds fail before filesystem access");
    sender.send(&codec::request(&request).expect("request")).expect("sent");
    let frame = receiver.receive().expect("request frame");
    let decoded = codec::read_request(&frame).expect("request decodes");
    let reply = session.dispatch(&decoded, &services(&host)).expect("page allowed");
    receiver
        .send(&codec::reply(&reply).expect("reply"))
        .expect("reply sent");
    let answer = codec::read_reply(&sender.receive().expect("reply frame")).expect("reply decodes");
    let Reply::DirectoryPage(page) = answer else {
        panic!("unexpected reply")
    };
    assert_eq!(page.next.expect("cursor").as_str(), "src/b.ts");
    assert!(page.more);
    assert_eq!(host.filesystem_pages.get(), 1);
}

#[test]
fn large_file_range_beyond_the_old_ceiling_crosses_the_real_socket() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("indexer").expect("name"),
        Grant::new([Capability::FilesystemRead]),
        Vec::new(),
    ))
    .with_filesystem(hl_extension::FilesystemGrant {
        read: vec![hl_extension::FilesystemSelector::Exact {
            exact: RelativePath::new("data/embeddings.bin").expect("root"),
        }],
        ..hl_extension::FilesystemGrant::default()
    });
    let request = Request::FilesystemReadRange {
        path: RelativePath::new("data/embeddings.bin").expect("path"),
        offset: 8 * 1024 * 1024,
        limit: 3,
        observed: Some("large-v1".into()),
    };
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);
    sender.send(&codec::request(&request).expect("request")).expect("sent");
    let decoded = codec::read_request(&receiver.receive().expect("request frame")).expect("decoded");
    let reply = session
        .dispatch(&decoded, &services(&host))
        .expect("large offset allowed");
    receiver
        .send(&codec::reply(&reply).expect("reply"))
        .expect("reply sent");
    let Reply::FileRange(range) = codec::read_reply(&sender.receive().expect("reply frame")).expect("reply decodes")
    else {
        panic!("unexpected reply")
    };
    assert_eq!(range.offset, 8 * 1024 * 1024);
    assert_eq!(range.contents, b"xxx");
    assert_eq!(
        host.filesystem_ranges.get(),
        1,
        "the request reached the filesystem exactly once"
    );
}

impl hl_extension::port::ExtensionStore for Host {}
impl hl_extension::NotificationSink for Host {
    fn publish(&self, _notification: &hl_extension::Notification) -> Result<(), HostError> {
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

/// Records what the adapter was told to draw, so a tree that was populated
/// without the renderer ever hearing about it would be visible.
#[derive(Default)]
struct Journal {
    patches: usize,
    commits: Vec<u64>,
}

impl hl_gui::Renderer for Journal {
    type Error = std::convert::Infallible;

    fn patch(&mut self, _patch: &Patch, _tree: &Tree) -> Result<(), Self::Error> {
        self.patches += 1;
        Ok(())
    }

    fn commit(&mut self, sequence: u64) -> Result<(), Self::Error> {
        self.commits.push(sequence);
        Ok(())
    }

    fn rows(&mut self, _window: &RowWindow) -> Result<(), Self::Error> {
        Ok(())
    }

    fn theme(&mut self, _theme: &Theme) -> Result<(), Self::Error> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// What the extension describes, stated independently of the tree.
// ---------------------------------------------------------------------------

/// One node as the extension asked for it.
///
/// Recorded beside the composition rather than read back out of a tree, so the
/// host's assertions compare the delivered interface against a statement of
/// intent that never went near the transport.
struct Expectation {
    id: NodeId,
    tag: hl_gui::Tag,
    props: Vec<(Prop, PropValue)>,
    handlers: Vec<(Trigger, EventId)>,
    children: Vec<NodeId>,
}

/// A surface being composed, alongside the record of what was asked for.
struct Composition {
    surface: Surface,
    expected: Vec<Expectation>,
    root: Vec<NodeId>,
}

impl Composition {
    fn new() -> Self {
        Self {
            surface: Surface::new(),
            expected: Vec::new(),
            root: Vec::new(),
        }
    }

    fn node(&mut self, tag: hl_gui::Tag, props: Vec<(Prop, PropValue)>, handlers: Vec<(Trigger, EventId)>) -> NodeId {
        let id = self.surface.create(tag);
        for (prop, value) in &props {
            self.surface.set(id, *prop, value.clone());
        }
        for (trigger, event) in &handlers {
            self.surface.on(id, *trigger, event.clone());
        }
        self.expected.push(Expectation {
            id,
            tag,
            props,
            handlers,
            children: Vec::new(),
        });
        id
    }

    fn plain(&mut self, tag: hl_gui::Tag) -> NodeId {
        self.node(tag, Vec::new(), Vec::new())
    }

    fn attach(&mut self, parent: NodeId, child: NodeId) {
        self.surface.append(parent, child);
        if parent == NodeId::ROOT {
            self.root.push(child);
            return;
        }
        let row = self
            .expected
            .iter_mut()
            .find(|node| node.id == parent)
            .expect("a parent must be described before it is attached to");
        row.children.push(child);
    }
}

/// The frames one conversation carries, with the description to check them
/// against. A pure function, so both sides can compute it without sharing.
struct Interface {
    panel: hl_gui::Frame,
    catalogue: hl_gui::Frame,
    expected: Vec<Expectation>,
    root: Vec<NodeId>,
}

fn text(value: &str) -> PropValue {
    PropValue::text(value)
}

/// Composes a panel using many component tags and real property values, then a
/// second frame holding one node of every tag the library defines.
fn interface() -> Interface {
    let mut composition = Composition::new();
    let card = panel(&mut composition);
    composition.attach(NodeId::ROOT, card);
    let panel = composition.surface.frame();

    let mut catalogue_nodes = Vec::new();
    for tag in hl_gui::Tag::ALL {
        catalogue_nodes.push(composition.plain(*tag));
    }
    for id in catalogue_nodes {
        composition.attach(NodeId::ROOT, id);
    }
    let catalogue = composition.surface.frame();

    Interface {
        panel,
        catalogue,
        expected: composition.expected,
        root: composition.root,
    }
}

/// The panel itself: a toolbar, a tabbed body, a table, and a footer.
fn panel(composition: &mut Composition) -> NodeId {
    let card = composition.node(
        hl_gui::Tag::Card,
        vec![
            (Prop::Pad, PropValue::Length(Length::Step(3))),
            (Prop::Variant, PropValue::Variant(Variant::Outline)),
        ],
        Vec::new(),
    );

    let toolbar = toolbar(composition);
    composition.attach(card, toolbar);
    let rule = composition.plain(hl_gui::Tag::Separator);
    composition.attach(card, rule);
    let body = body(composition);
    composition.attach(card, body);
    let footer = footer(composition);
    composition.attach(card, footer);
    card
}

fn toolbar(composition: &mut Composition) -> NodeId {
    let toolbar = composition.node(
        hl_gui::Tag::Toolbar,
        vec![
            (Prop::Gap, PropValue::Length(Length::Step(2))),
            (Prop::Pad, PropValue::Length(Length::Step(1))),
        ],
        Vec::new(),
    );

    let heading = composition.node(
        hl_gui::Tag::Heading,
        vec![
            (Prop::Label, text("Containers")),
            (Prop::Scale, PropValue::Scale(Scale::Title)),
        ],
        Vec::new(),
    );
    composition.attach(toolbar, heading);

    let icon = composition.node(
        hl_gui::Tag::Icon,
        vec![
            (Prop::Label, text("view-refresh")),
            (Prop::Tone, PropValue::Tone(Tone::Accent)),
        ],
        Vec::new(),
    );
    composition.attach(toolbar, icon);

    let spacer = composition.node(
        hl_gui::Tag::Spacer,
        vec![(Prop::Width, PropValue::Length(Length::Fill))],
        Vec::new(),
    );
    composition.attach(toolbar, spacer);

    let search = composition.node(
        hl_gui::Tag::Entry,
        vec![
            (Prop::Value, text("alpine")),
            (Prop::Width, PropValue::Length(Length::Chars(24))),
        ],
        vec![(Trigger::Change, EventId::new("filter.changed"))],
    );
    composition.attach(toolbar, search);

    let restart = composition.node(
        hl_gui::Tag::Button,
        vec![
            (Prop::Label, text("Restart")),
            (Prop::Variant, PropValue::Variant(Variant::Filled)),
            (Prop::Tone, PropValue::Tone(Tone::Danger)),
        ],
        vec![(Trigger::Invoke, EventId::new("container.restart"))],
    );
    composition.attach(toolbar, restart);
    toolbar
}

fn body(composition: &mut Composition) -> NodeId {
    let tabs = composition.node(
        hl_gui::Tag::Tabs,
        vec![(Prop::Gap, PropValue::Length(Length::Step(2)))],
        vec![(Trigger::Select, EventId::new("tab.selected"))],
    );

    let column = composition.node(
        hl_gui::Tag::Column,
        vec![
            (Prop::Gap, PropValue::Length(Length::Step(2))),
            (Prop::Align, PropValue::Align(Align::Stretch)),
        ],
        Vec::new(),
    );
    composition.attach(tabs, column);

    let table = composition.node(
        hl_gui::Tag::DataTable,
        vec![
            (Prop::Source, PropValue::Source(SourceId::new(7))),
            (
                Prop::Schema,
                PropValue::Schema(vec![
                    TableColumn::new("name", "Name").width(Length::Fill).sortable(),
                    TableColumn::new("state", "State").width(Length::Chars(12)),
                ]),
            ),
        ],
        vec![(Trigger::Activate, EventId::new("row.activated"))],
    );
    composition.attach(column, table);

    let expander = composition.node(
        hl_gui::Tag::Expander,
        vec![(Prop::Label, text("Advanced"))],
        vec![(Trigger::Expand, EventId::new("advanced.toggled"))],
    );
    composition.attach(column, expander);

    let settings = settings(composition);
    composition.attach(expander, settings);
    tabs
}

fn settings(composition: &mut Composition) -> NodeId {
    let row = composition.node(
        hl_gui::Tag::Row,
        vec![(Prop::Gap, PropValue::Length(Length::Step(2)))],
        Vec::new(),
    );

    let label = composition.node(
        hl_gui::Tag::Text,
        vec![(Prop::Label, text("Restart policy"))],
        Vec::new(),
    );
    composition.attach(row, label);

    let select = composition.node(
        hl_gui::Tag::Select,
        vec![
            (Prop::Value, text("always")),
            (
                Prop::Choices,
                PropValue::Choices(vec![Choice::new("always", "Always"), Choice::new("never", "Never")]),
            ),
        ],
        vec![(Trigger::Select, EventId::new("policy.chosen"))],
    );
    composition.attach(row, select);

    let switch = composition.node(
        hl_gui::Tag::Switch,
        vec![(Prop::Checked, PropValue::Flag(true))],
        vec![(Trigger::Toggle, EventId::new("autostart.toggled"))],
    );
    composition.attach(row, switch);
    row
}

fn footer(composition: &mut Composition) -> NodeId {
    let row = composition.node(
        hl_gui::Tag::Row,
        vec![
            (Prop::Gap, PropValue::Length(Length::Step(1))),
            (Prop::Pad, PropValue::Length(Length::Step(1))),
        ],
        Vec::new(),
    );

    let badge = composition.node(
        hl_gui::Tag::Badge,
        vec![
            (Prop::Label, text("running")),
            (Prop::Tone, PropValue::Tone(Tone::Positive)),
        ],
        Vec::new(),
    );
    composition.attach(row, badge);

    let progress = composition.node(
        hl_gui::Tag::Progress,
        vec![
            (Prop::Fraction, PropValue::Number(0.42)),
            (Prop::Width, PropValue::Length(Length::Fill)),
        ],
        Vec::new(),
    );
    composition.attach(row, progress);

    let separator = composition.node(hl_gui::Tag::Separator, Vec::new(), Vec::new());
    composition.attach(row, separator);
    row
}

// ---------------------------------------------------------------------------
// The two sides.
// ---------------------------------------------------------------------------

/// One call and its answer, from the extension's side.
fn call(wire: &mut hl_extension::Wire<Stream>, request: &Request) -> Result<Reply, Failure> {
    wire.send(&codec::request(request).expect("the call encodes"))
        .expect("sent");
    let frame = wire.receive().expect("an answer");
    if codec::is_failure(&frame) {
        return Err(codec::read_failure(&frame).expect("the failure decodes"));
    }
    Ok(codec::read_reply(&frame).expect("the reply decodes"))
}

/// Everything the extension process does, start to finish.
fn extension(stream: Stream) -> Result<(), String> {
    let mut wire = hl_extension::Wire::new(stream);

    let opening = wire.receive().map_err(|error| error.to_string())?;
    let welcome = codec::read_welcome(&opening).map_err(|error| error.to_string())?;
    if welcome.protocol != PROTOCOL {
        return Err(format!("the host speaks protocol {}", welcome.protocol));
    }
    wire.send(
        &codec::hello(&Hello {
            protocol: PROTOCOL,
            name: welcome.peer.clone(),
            features: vec!["interface".into()],
        })
        .expect("the greeting encodes"),
    )
    .map_err(|error| error.to_string())?;

    let composed = interface();
    let premature = call(
        &mut wire,
        &Request::InterfaceRender {
            frame: hl_gui::Surface::new().frame(),
        },
    );
    if !matches!(premature, Err(Failure::Conflict { .. })) {
        return Err(format!("drawing before opening a tab gave {premature:?}"));
    }

    call(
        &mut wire,
        &Request::InterfaceOpenTab {
            title: "Containers".into(),
        },
    )
    .map_err(|failure| format!("the tab was refused: {failure:?}"))?;
    call(&mut wire, &Request::InterfaceRender { frame: composed.panel })
        .map_err(|failure| format!("the panel was refused: {failure:?}"))?;
    call(
        &mut wire,
        &Request::InterfaceRender {
            frame: composed.catalogue,
        },
    )
    .map_err(|failure| format!("the catalogue was refused: {failure:?}"))?;
    Ok(())
}

/// Answers one call, keeping the host's tree in step with what it accepted.
fn turn(
    wire: &mut hl_extension::Wire<Stream>,
    session: &mut Session,
    host: &Host,
    tree: &mut Tree,
    journal: &mut Journal,
) -> Result<Reply, Failure> {
    let frame = wire.receive().expect("a call from the extension");
    let request = codec::read_request(&frame).expect("the call decodes");
    let outcome = session.dispatch(&request, &services(host));
    let answer = match &outcome {
        Ok(reply) => codec::reply(reply).expect("the reply encodes"),
        Err(failure) => codec::failure(failure).expect("the failure encodes"),
    };
    wire.send(&answer).expect("the answer is sent");
    for pending in session.drain() {
        tree.apply(&pending.frame, journal)
            .expect("the host applies what it accepted");
    }
    outcome
}

fn session() -> Session {
    Session::new(Authority::new(
        ExtensionName::new("containers").expect("name"),
        Grant::new([Capability::Interface]),
        Vec::new(),
    ))
}

// ---------------------------------------------------------------------------
// Assertions over the reconstructed tree.
// ---------------------------------------------------------------------------

fn compare(tree: &Tree, expectation: &Expectation) {
    let node = tree.node(expectation.id).unwrap_or_else(|| {
        panic!(
            "node {} ({}) never arrived",
            expectation.id.raw(),
            expectation.tag.as_str()
        )
    });

    assert_eq!(
        node.tag,
        expectation.tag,
        "node {} arrived as {} but was described as {}",
        expectation.id.raw(),
        node.tag.as_str(),
        expectation.tag.as_str()
    );
    compare_props(node, expectation);
    compare_handlers(node, expectation);
    assert_eq!(
        node.children,
        expectation.children,
        "the children of {} node {} do not match",
        expectation.tag.as_str(),
        expectation.id.raw()
    );
}

fn compare_props(node: &hl_gui::Node, expectation: &Expectation) {
    assert_eq!(
        node.props.len(),
        expectation.props.len(),
        "{} node {} arrived with {:?}, described as {:?}",
        expectation.tag.as_str(),
        expectation.id.raw(),
        node.props,
        expectation.props
    );
    for (prop, value) in &expectation.props {
        assert_eq!(
            node.prop(*prop),
            Some(value),
            "{prop:?} on {} node {} did not survive",
            expectation.tag.as_str(),
            expectation.id.raw()
        );
    }
}

fn compare_handlers(node: &hl_gui::Node, expectation: &Expectation) {
    assert_eq!(
        node.handlers.len(),
        expectation.handlers.len(),
        "{} node {} arrived with handlers {:?}, described as {:?}",
        expectation.tag.as_str(),
        expectation.id.raw(),
        node.handlers,
        expectation.handlers
    );
    for (trigger, event) in &expectation.handlers {
        assert_eq!(
            node.handler(*trigger),
            Some(event),
            "the {trigger:?} handler on {} node {} did not survive",
            expectation.tag.as_str(),
            expectation.id.raw()
        );
    }
}

// ---------------------------------------------------------------------------
// The conversation.
// ---------------------------------------------------------------------------

#[test]
fn a_whole_interface_is_rendered_from_a_socket() {
    let (host_end, extension_end) = connected_pair();
    let speaker = std::thread::spawn(move || extension(extension_end));

    let host = Host::new();
    let mut session = session();
    let mut tree = Tree::new();
    let mut journal = Journal::default();
    let mut wire = hl_extension::Wire::new(host_end);

    wire.send(
        &codec::welcome(&Welcome {
            protocol: PROTOCOL,
            host: "husklet".into(),
            workspace: "dev".into(),
            peer: ExtensionName::new("containers").expect("name"),
            granted: Grant::new([Capability::Interface]),
            filesystem: hl_extension::FilesystemGrant::default(),
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
            limits: hl_extension::Limits::default(),
        })
        .expect("the welcome encodes"),
    )
    .expect("sent");
    let greeting = codec::read_hello(&wire.receive().expect("a greeting")).expect("the greeting decodes");
    assert_eq!(greeting.protocol, PROTOCOL);
    assert_eq!(greeting.features, vec!["interface".to_owned()]);

    let premature = turn(&mut wire, &mut session, &host, &mut tree, &mut journal).expect_err("nowhere to draw");
    assert!(
        matches!(premature, Failure::Conflict { .. }),
        "a frame with no tab open must be refused as a conflict, got {premature:?}"
    );
    assert!(tree.is_empty(), "a refused frame must not reach the tree");

    for _ in 0..3 {
        turn(&mut wire, &mut session, &host, &mut tree, &mut journal).expect("accepted");
    }
    assert_eq!(session.tab(), Some("tab-Containers"));

    speaker
        .join()
        .expect("the extension thread finished")
        .expect("the extension is content");
    assert_eq!(
        wire.receive().expect_err("the extension is gone"),
        Transit::Closed,
        "an extension exiting is the ordinary end of a session"
    );

    let composed = interface();
    for expectation in &composed.expected {
        compare(&tree, expectation);
    }
    assert_eq!(
        tree.root().children,
        composed.root,
        "the top level of the reconstructed interface differs"
    );
    assert_eq!(
        tree.len(),
        composed.expected.len() + 1,
        "the host's tree holds nodes the extension never described"
    );
    assert_eq!(journal.commits, vec![1, 2], "each accepted frame is presented once");
    assert!(
        journal.patches >= composed.expected.len(),
        "every node reached the adapter"
    );
}

#[test]
fn container_name_boundaries_cross_the_real_socket_before_dispatch() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("containers").expect("name"),
        Grant::new([
            Capability::ContainerCreate,
            Capability::VolumeWrite,
            Capability::NetworkConnect,
        ]),
        Vec::new(),
    ))
    .with_containers(hl_extension::ContainerGrant {
        selectors: Vec::new(),
        create: true,
    })
    .with_images(hl_extension::ImageGrant {
        r#use: vec![hl_extension::ImageSelector::Reference {
            reference: "docker.io/library/alpine:3.20".into(),
        }],
        ..hl_extension::ImageGrant::default()
    })
    .with_volumes(hl_extension::VolumeGrant {
        selectors: vec![hl_extension::VolumeSelector::Name { name: "v".repeat(255) }],
        create: false,
    })
    .with_networks(hl_extension::NetworkGrant {
        selectors: vec![hl_extension::NetworkSelector::Name { name: "n".repeat(255) }],
        create: false,
    });
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);
    let request = Request::ContainerCreate {
        spec: ContainerCreateSpec {
            image: "docker.io/library/alpine:3.20".into(),
            name: "worker".into(),
            hostname: Some("h".repeat(253)),
            entrypoint: None,
            command: Vec::new(),
            environment: vec![("é".repeat(128), "value".into())],
            working_directory: None,
            user: None,
            labels: Vec::new(),
            mounts: vec![ContainerVolumeMount {
                volume: "v".repeat(255),
                target: "/data".into(),
                read_only: false,
            }],
            network: Some("n".repeat(255)),
            ports: Vec::new(),
            memory_mb: None,
            cpus: None,
            pids_limit: None,
        },
    };
    sender
        .send(&codec::request(&request).expect("request encodes"))
        .expect("request crosses socket");
    let decoded = codec::read_request(&receiver.receive().expect("request arrives")).expect("request decodes");
    assert_eq!(decoded, request);
    assert!(matches!(
        session.dispatch(&decoded, &services(&host)),
        Err(Failure::Unsupported { call }) if call == "configured container creation is unavailable"
    ));
}

#[test]
fn terminal_screen_grid_and_cursor_cross_the_real_socket_together() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("terminal-reader").expect("name"),
        Grant::new([Capability::TerminalOutput]),
        Vec::new(),
    ));
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);
    let request = Request::TerminalReadPane {
        slot: "pane-7".into(),
        lines: Some(20),
    };
    sender
        .send(&codec::request(&request).expect("request encodes"))
        .expect("request crosses socket");
    let decoded = codec::read_request(&receiver.receive().expect("request arrives")).expect("request decodes");
    let Reply::Text(screen) = session.dispatch(&decoded, &services(&host)).expect("screen read") else {
        panic!("wrong terminal reply")
    };
    assert_eq!((screen.columns, screen.rows), (120, 40));
    assert_eq!((screen.cursor_column, screen.cursor_row), (12, 3));
    receiver
        .send(&codec::reply(&Reply::Text(screen.clone())).expect("reply encodes"))
        .expect("reply crosses socket");
    let reply = codec::read_reply(&sender.receive().expect("reply arrives")).expect("reply decodes");
    assert_eq!(reply, Reply::Text(screen));
}

#[test]
fn legacy_and_maximal_network_alias_calls_cross_a_real_socket() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("networks").unwrap(),
        Grant::new([Capability::NetworkConnect]),
        Vec::new(),
    ))
    .with_containers(hl_extension::ContainerGrant {
        selectors: vec![hl_extension::ContainerSelector::Id { id: "b".repeat(64) }],
        create: false,
    })
    .with_networks(hl_extension::NetworkGrant {
        selectors: vec![hl_extension::NetworkSelector::All { all: true }],
        create: false,
    });
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);
    let legacy: Request = serde_json::from_str(&format!(
        "{{\"call\":\"network_connect\",\"with\":{{\"reference\":\"{}\",\"container\":\"{}\"}}}}",
        "a".repeat(32),
        "b".repeat(64)
    ))
    .unwrap();
    assert!(matches!(&legacy, Request::NetworkConnect { aliases, .. } if aliases.is_empty()));
    for request in [legacy, {
        let mut aliases = (0..64).map(|index| format!("alias-{index}")).collect::<Vec<_>>();
        aliases[0] = "x".repeat(253);
        Request::NetworkConnect {
            reference: "a".repeat(32),
            container: "b".repeat(64),
            aliases,
        }
    }] {
        sender.send(&codec::request(&request).unwrap()).unwrap();
        let decoded = codec::read_request(&receiver.receive().unwrap()).unwrap();
        session.dispatch(&decoded, &services(&host)).unwrap();
        if let Request::NetworkConnect { aliases, .. } = request {
            assert_eq!(*host.network_aliases.borrow(), aliases);
        }
    }
}

#[test]
fn network_connect_authority_cannot_remove_a_network_over_a_real_socket() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("postgres-inspector").unwrap(),
        Grant::new([Capability::NetworkConnect]),
        Vec::new(),
    ))
    .with_networks(hl_extension::NetworkGrant {
        selectors: vec![hl_extension::NetworkSelector::Id { id: "a".repeat(32) }],
        create: false,
    });
    let request = Request::NetworkRemove {
        reference: "a".repeat(32),
    };
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);

    sender
        .send(&codec::request(&request).expect("request encodes"))
        .expect("request sent");
    let decoded = codec::read_request(&receiver.receive().expect("request arrives")).expect("request decodes");
    let failure = session
        .dispatch(&decoded, &services(&host))
        .expect_err("connect-only authority must not remove a network");
    receiver
        .send(&codec::failure(&failure).expect("failure encodes"))
        .expect("failure sent");

    assert!(matches!(
        codec::read_failure(&sender.receive().expect("failure arrives")),
        Ok(Failure::Denied { capability, .. }) if capability == Capability::NetworkRemove.as_str()
    ));
}

#[test]
fn network_connect_authority_cannot_detach_an_endpoint_over_a_real_socket() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("postgres-inspector").unwrap(),
        Grant::new([Capability::NetworkConnect]),
        Vec::new(),
    ));
    let request = Request::NetworkDisconnect {
        reference: "a".repeat(32),
        container: "c".repeat(64),
    };
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);

    sender.send(&codec::request(&request).unwrap()).unwrap();
    let decoded = codec::read_request(&receiver.receive().unwrap()).unwrap();
    let failure = session
        .dispatch(&decoded, &services(&host))
        .expect_err("temporary attachment authority must not imply cleanup authority");
    receiver.send(&codec::failure(&failure).unwrap()).unwrap();

    assert!(matches!(
        codec::read_failure(&sender.receive().unwrap()),
        Ok(Failure::Denied { capability, .. }) if capability == Capability::NetworkDisconnect.as_str()
    ));
    assert!(host.network_aliases.borrow().is_empty(), "network adapter was not reached");
}

#[test]
fn network_connect_cannot_cross_an_ungranted_container_scope_over_a_real_socket() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("networks").unwrap(),
        Grant::new([Capability::NetworkConnect]),
        Vec::new(),
    ))
    .with_containers(hl_extension::ContainerGrant {
        selectors: vec![hl_extension::ContainerSelector::Id { id: "c".repeat(64) }],
        create: false,
    })
    .with_networks(hl_extension::NetworkGrant {
        selectors: vec![hl_extension::NetworkSelector::All { all: true }],
        create: false,
    });
    let request = Request::NetworkConnect {
        reference: "a".repeat(32),
        container: "b".repeat(64),
        aliases: vec!["database".into()],
    };
    let mut sender = hl_extension::Wire::new(extension_end);
    let mut receiver = hl_extension::Wire::new(host_end);

    sender.send(&codec::request(&request).expect("request")).expect("sent");
    let decoded = codec::read_request(&receiver.receive().expect("request frame")).expect("decoded");
    let failure = session
        .dispatch(&decoded, &services(&host))
        .expect_err("a different container selector cannot be exceeded");
    receiver
        .send(&codec::failure(&failure).expect("failure frame"))
        .expect("failure sent");

    assert!(matches!(
        codec::read_failure(&sender.receive().expect("failure reply")),
        Ok(Failure::Denied { capability, .. }) if capability == Capability::NetworkConnect.as_str()
    ));
    assert!(
        host.network_aliases.borrow().is_empty(),
        "the network adapter was not reached across the consent boundary"
    );
}

#[test]
fn hidden_execution_output_is_denied_over_unix_framing_and_the_session_stays_healthy() {
    let (host_end, extension_end) = connected_pair();
    let host = Host::new();
    let mut session = Session::new(Authority::new(
        ExtensionName::new("database").unwrap(),
        Grant::new([Capability::ContainerRead, Capability::WorkspaceRead]),
        Vec::new(),
    ))
    .with_containers(hl_extension::ContainerGrant {
        selectors: vec![hl_extension::ContainerSelector::Name {
            name: "postgres".into(),
        }],
        create: false,
    });
    let mut extension = hl_extension::Wire::new(extension_end);
    let mut server = hl_extension::Wire::new(host_end);

    extension
        .send(
            &codec::request(&Request::ExecutionOutput {
                id: "e".repeat(32),
                after: 0,
                limit: 16,
            })
            .expect("output request"),
        )
        .expect("output request sent");
    let request = codec::read_request(&server.receive().expect("output frame")).expect("output decoded");
    let failure = session
        .dispatch(&request, &services(&host))
        .expect_err("hidden execution denied");
    server
        .send(&codec::failure(&failure).expect("failure frame"))
        .expect("failure sent");
    assert!(matches!(
        codec::read_failure(&extension.receive().expect("failure received")),
        Ok(Failure::Denied { capability, .. }) if capability == Capability::ContainerRead.as_str()
    ));
    assert_eq!(host.execution_output_calls.get(), 0);

    extension
        .send(&codec::request(&Request::WorkspaceInfo).expect("health request"))
        .expect("health request sent");
    let request = codec::read_request(&server.receive().expect("health frame")).expect("health decoded");
    let reply = session
        .dispatch(&request, &services(&host))
        .expect("session remains healthy");
    server
        .send(&codec::reply(&reply).expect("health reply"))
        .expect("health reply sent");
    assert!(matches!(
        codec::read_reply(&extension.receive().expect("health reply received")),
        Ok(Reply::Workspace(info)) if info.name == "dev"
    ));
}

#[test]
fn every_component_tag_can_be_sent_and_reconstructed() {
    let composed = interface();
    let mut tree = Tree::new();
    let mut journal = Journal::default();
    tree.apply(&composed.panel, &mut journal).expect("the panel applies");
    tree.apply(&composed.catalogue, &mut journal)
        .expect("the catalogue applies");

    for tag in hl_gui::Tag::ALL {
        let found = composed
            .expected
            .iter()
            .filter(|expectation| expectation.tag == *tag)
            .find_map(|expectation| tree.node(expectation.id));
        assert!(
            found.is_some_and(|node| node.tag == *tag),
            "{} could not be sent and reconstructed",
            tag.as_str()
        );
    }
}

#[test]
fn a_message_too_large_to_send_is_refused_rather_than_framed() {
    let mut surface = Surface::new();
    for _ in 0..40_000 {
        let node = surface.text("a label long enough that forty thousand of them exceed the payload limit");
        surface.append(NodeId::ROOT, node);
    }
    let request = Request::InterfaceRender { frame: surface.frame() };

    let refusal = codec::request(&request).expect_err("refused");

    match refusal {
        Coding::Oversize(length) => assert!(length > hl_extension::Frame::PAYLOAD_LIMIT),
        other @ Coding::Malformed(_) => {
            panic!("an interface too large to send must be refused as oversize, got {other}")
        }
    }
}

#[test]
fn a_frame_of_the_wrong_kind_is_not_read_as_a_message() {
    let reply = codec::reply(&Reply::Done).expect("encoded");
    assert!(codec::read_request(&reply).is_err(), "a reply is not a call");
    assert!(
        codec::read_welcome(&reply).is_err(),
        "a call channel frame is not a welcome"
    );

    let failure = codec::failure(&Failure::Absent { detail: "gone".into() }).expect("encoded");
    assert!(
        codec::read_reply(&failure).is_err(),
        "a failure must not be parsed as a result"
    );
    assert!(
        codec::read_failure(&reply).is_err(),
        "a result must not be parsed as a failure"
    );
    assert_eq!(
        codec::read_failure(&failure).expect("decoded"),
        Failure::Absent { detail: "gone".into() }
    );

    let unavailable = codec::failure(&Failure::Unavailable {
        detail: "socket refused".into(),
    })
    .expect("unavailable encoded");
    assert_eq!(
        codec::read_failure(&unavailable).expect("unavailable decoded"),
        Failure::Unavailable {
            detail: "socket refused".into()
        }
    );
}
