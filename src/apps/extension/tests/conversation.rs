//! The extension's half of a real conversation.
//!
//! A scripted host drives the extension across a socket pair and checks what it
//! says: the order it says it in, the interface it describes, and the rows it
//! answers with. Nothing here shares code with the extension's own view, so a
//! change in either has to be reconciled deliberately.

/// Calls to read before giving up on an extension that never finishes.
const EXCHANGES: usize = 8;

use hl_extension::port::ContainerSummary;
use hl_extension::{
    ChannelId, ExtensionName, Frame, Grant, Kind, Limits, Reply, Request, Transit, Welcome, Wire, PROTOCOL,
};
use hl_gui::{Patch, Prop, PropValue, RequestId, RowRange, RowRequest, Tag, Tree, Version};

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

/// Runs the extension against a scripted host and returns everything it said.
fn converse(exchanges: usize) -> Vec<Request> {
    let (ours, theirs) = connected_pair();
    let extension = std::thread::spawn(move || extension::serve(theirs, extension::Extension::new()));

    let mut wire = Wire::new(ours);
    greet(&mut wire);
    let said = listen(&mut wire, exchanges);

    drop(wire);
    let outcome = extension.join().expect("the extension thread");
    assert_eq!(outcome, Ok(()), "a host hangup is a clean end, not a failure");
    said
}

/// Sends the opening frame and reads the extension's reply.
fn greet(wire: &mut Wire<Stream>) {
    let welcome = Welcome {
        protocol: PROTOCOL,
        host: "test".into(),
        workspace: "dev".into(),
        peer: ExtensionName::new("containers").expect("a name"),
        granted: Grant::new([
            hl_extension::Capability::ContainerRead,
            hl_extension::Capability::Interface,
        ]),
        limits: Limits::default(),
    };
    send(wire, Kind::Request, &welcome);
    let hello = wire.receive().expect("a greeting");
    let hello: hl_extension::Hello = serde_json::from_slice(&hello.payload).expect("a hello");
    assert_eq!(hello.protocol, PROTOCOL, "the extension must state its protocol");
}

/// Reads calls, answering each so the extension can continue.
///
/// Stops once the extension has said how long its table is, which is the last
/// thing it says unprompted. Reading past that blocks: both sides are then
/// waiting on the other.
fn listen(wire: &mut Wire<Stream>, exchanges: usize) -> Vec<Request> {
    let mut said = Vec::new();
    for _ in 0..exchanges {
        let frame = match wire.receive() {
            Ok(frame) => frame,
            Err(Transit::Closed) => break,
            Err(other) => panic!("the extension stopped talking: {other}"),
        };
        let request: Request = serde_json::from_slice(&frame.payload).expect("a call");
        let reply = answer(&request);
        let complete = matches!(request, Request::SourceResize { .. });
        said.push(request);
        send(wire, Kind::Response, &reply);
        if complete {
            break;
        }
    }
    said
}

/// What the scripted host replies with.
fn answer(request: &Request) -> Reply {
    match request {
        Request::InterfaceOpenTab { title } => Reply::Identity(format!("tab-{title}")),
        Request::ContainerList => Reply::Containers(containers()),
        _ => Reply::Done,
    }
}

fn containers() -> Vec<ContainerSummary> {
    [("api", "running"), ("db", "exited"), ("cache", "restarting")]
        .into_iter()
        .enumerate()
        .map(|(index, (name, state))| ContainerSummary {
            id: format!("c{index}"),
            name: name.into(),
            image: "alpine:3.20".into(),
            state: state.into(),
            created: 1_700_000_000,
        })
        .collect()
}

fn send<T: serde::Serialize>(wire: &mut Wire<Stream>, kind: Kind, value: &T) {
    let payload = serde_json::to_vec(value).expect("serialized");
    wire.send(&Frame::new(ChannelId::new(1), kind, payload)).expect("sent");
}

#[test]
fn the_extension_opens_a_tab_before_it_draws_anything() {
    let said = converse(8);

    let opened = said
        .iter()
        .position(|request| matches!(request, Request::InterfaceOpenTab { .. }))
        .expect("a tab is opened");
    let drawn = said
        .iter()
        .position(|request| matches!(request, Request::InterfaceRender { .. }))
        .expect("an interface is drawn");

    assert!(
        opened < drawn,
        "an interface must have somewhere to go before it is sent"
    );
}

#[test]
fn the_extension_subscribes_before_it_asks_for_a_listing() {
    let said = converse(8);

    let subscribed = said
        .iter()
        .position(|request| matches!(request, Request::EventSubscribe { .. }))
        .expect("a subscription");
    let listed = said
        .iter()
        .position(|request| matches!(request, Request::ContainerList))
        .expect("a listing");

    assert!(
        subscribed < listed,
        "subscribing after listing would miss anything that changed in between"
    );
}

#[test]
fn the_described_interface_applies_to_a_tree_and_holds_what_it_should() {
    let said = converse(8);

    let frame = said
        .iter()
        .find_map(|request| match request {
            Request::InterfaceRender { frame } => Some(frame.clone()),
            _ => None,
        })
        .expect("an interface");

    let mut tree = Tree::new();
    tree.apply(&frame, &mut Ignore).expect("the interface is well formed");

    let tags: Vec<Tag> = frame
        .patches
        .iter()
        .filter_map(|patch| match patch {
            Patch::Create { tag, .. } => Some(*tag),
            _ => None,
        })
        .collect();
    for expected in [
        Tag::Toolbar,
        Tag::Heading,
        Tag::Badge,
        Tag::Search,
        Tag::Button,
        Tag::DataTable,
    ] {
        assert!(tags.contains(&expected), "{expected:?} is missing from the interface");
    }
    assert!(!tree.is_empty(), "the interface reaches the root");
}

#[test]
fn the_extension_reports_how_long_its_table_is() {
    let said = converse(8);

    let rows = said
        .iter()
        .find_map(|request| match request {
            Request::SourceResize {
                mutation: hl_gui::SourceMutation::Length { rows, .. },
            } => Some(*rows),
            _ => None,
        })
        .expect("a length");

    assert_eq!(rows, containers().len() as u64, "the table describes the listing");
}

#[test]
fn the_extension_answers_a_row_window_with_the_containers_it_was_given() {
    let (ours, theirs) = connected_pair();
    let extension = std::thread::spawn(move || extension::serve(theirs, extension::Extension::new()));
    let mut wire = Wire::new(ours);
    greet(&mut wire);
    let _ = listen(&mut wire, EXCHANGES);

    let request = RowRequest {
        id: RequestId::new(1),
        source: extension::SOURCE,
        version: Version::new(1),
        range: RowRange::new(0, 128),
        sort: None,
        filter: None,
    };
    let payload = serde_json::to_vec(&request).expect("serialized");
    wire.send(&Frame::new(ChannelId::new(3), Kind::Event, payload))
        .expect("sent");
    let frame = wire.receive().expect("a window");
    let window: hl_gui::RowWindow = serde_json::from_slice(&frame.payload).expect("a window");

    assert_eq!(window.rows.len(), containers().len(), "every container is a row");
    assert_eq!(window.request, request.id, "the answer names the question");
    assert_eq!(window.source, extension::SOURCE);

    drop(wire);
    let _ = extension.join();
}

#[test]
fn the_interface_describes_its_table_by_source_rather_than_by_rows() {
    let said = converse(8);
    let frame = said
        .iter()
        .find_map(|request| match request {
            Request::InterfaceRender { frame } => Some(frame.clone()),
            _ => None,
        })
        .expect("an interface");

    let bound = frame.patches.iter().any(|patch| {
        matches!(
            patch,
            Patch::SetProp {
                prop: Prop::Source,
                value: PropValue::Source(source),
                ..
            } if *source == extension::SOURCE
        )
    });
    assert!(bound, "the table must name the source its rows come from");
}

/// A renderer for tests that only care whether a description is well formed.
struct Ignore;

impl hl_gui::Renderer for Ignore {
    type Error = std::convert::Infallible;

    fn patch(&mut self, _patch: &Patch, _tree: &Tree) -> Result<(), Self::Error> {
        Ok(())
    }

    fn commit(&mut self, _sequence: u64) -> Result<(), Self::Error> {
        Ok(())
    }

    fn rows(&mut self, _window: &hl_gui::RowWindow) -> Result<(), Self::Error> {
        Ok(())
    }

    fn theme(&mut self, _theme: &hl_gui::Theme) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn the_manifest_this_extension_ships_is_one_a_host_accepts() {
    let manifest = extension::manifest().expect("a manifest");

    let parsed = hl_extension::Manifest::parse(extension::DOCUMENT, PROTOCOL).expect("a host accepts it");

    assert_eq!(parsed, manifest, "the shipped document is the manifest");
    assert_eq!(parsed.name.as_str(), "containers");
    assert!(
        parsed.interface.is_some(),
        "an extension that draws must declare that it draws"
    );
}

#[test]
fn this_extension_asks_for_no_more_than_it_needs() {
    let manifest = extension::manifest().expect("a manifest");

    assert!(manifest.capabilities.holds(hl_extension::Capability::ContainerRead));
    assert!(manifest.capabilities.holds(hl_extension::Capability::Interface));
    assert!(
        !manifest.capabilities.executes(),
        "nothing this extension does needs to run code in the workspace"
    );
    assert!(
        manifest.filesystem_roots.is_empty(),
        "it reads no files, so it declares no roots and can reach none"
    );
}
