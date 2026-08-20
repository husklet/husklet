//! A whole extension conversation over a real socket.
//!
//! The two sides here are shaped like two processes: an extension on its own
//! thread that owns one end of a `UnixStream` and never touches the host's
//! state, and a host on the main thread that owns a [`Session`], in-memory
//! ports, and a real [`hl_gui::Tree`]. Nothing is shared but bytes.
//!
//! What this proves is the claim the whole protocol exists to make: an
//! interface composed out of process arrives whole. The assertions therefore
//! walk the reconstructed tree and compare tag, properties, handlers, and
//! parentage against what the extension described, because a test that only
//! checked for the absence of an error would pass on an empty tree.

use std::cell::RefCell;
use std::os::unix::net::UnixStream;

use hl_extension::port::{
    ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
    PaneText, TabSummary, TerminalSurface, WorkspaceFiles, WorkspaceInventory, WorkspaceState,
};
use hl_extension::{
    codec, Authority, Capability, Coding, ExtensionName, Failure, Grant, Hello, RelativePath, Reply, Request, Services,
    Session, Transit, Welcome, WorkspaceInfo, PROTOCOL,
};
use hl_gui::{
    Align, Choice, Column as TableColumn, EventId, Length, NodeId, Patch, Prop, PropValue, RowWindow, Scale, SourceId,
    Surface, Theme, Tone, Tree, Trigger, Variant,
};

// ---------------------------------------------------------------------------
// The host's in-memory ports.
// ---------------------------------------------------------------------------

/// Host services backed by nothing, so the conversation exercises the protocol
/// rather than a container runtime.
struct Host {
    tabs: RefCell<Vec<String>>,
}

impl Host {
    fn new() -> Self {
        Self {
            tabs: RefCell::new(Vec::new()),
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
}

impl ContainerControl for Host {
    fn create(&self, _image: &str, name: &str) -> Result<String, HostError> {
        Ok(format!("id-{name}"))
    }

    fn start(&self, _id: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn stop(&self, _id: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn remove(&self, _id: &str) -> Result<(), HostError> {
        Ok(())
    }
}

impl ImageStore for Host {
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        Ok(Vec::new())
    }

    fn pull(&self, reference: &str) -> Result<ImageSummary, HostError> {
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
    fn list(&self, _path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        Ok(Vec::new())
    }

    fn read(&self, _path: &RelativePath) -> Result<Vec<u8>, HostError> {
        Ok(Vec::new())
    }

    fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
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
fn call(wire: &mut hl_extension::Wire<UnixStream>, request: &Request) -> Result<Reply, Failure> {
    wire.send(&codec::request(request).expect("the call encodes"))
        .expect("sent");
    let frame = wire.receive().expect("an answer");
    if codec::is_failure(&frame) {
        return Err(codec::read_failure(&frame).expect("the failure decodes"));
    }
    Ok(codec::read_reply(&frame).expect("the reply decodes"))
}

/// Everything the extension process does, start to finish.
fn extension(stream: UnixStream) -> Result<(), String> {
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
    wire: &mut hl_extension::Wire<UnixStream>,
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
        tree.apply(&pending, journal)
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
    let (host_end, extension_end) = UnixStream::pair().expect("a socket pair");
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
}
