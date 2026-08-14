//! Hosts the reference extension over a real socket and renders what it sends.
//!
//! This is the end-to-end path in miniature: an extension in another thread,
//! speaking the protocol across a socket pair, describing an interface the
//! toolkit renders. Nothing about the interface is known here in advance.

use std::os::unix::net::UnixStream;

use hl_gui::{Renderer, Tree};
use hl_gui_gtk::Surface as Widgets;
use hl_ws_extension::port::ContainerSummary;
use hl_ws_extension::{
    Authority, Capability, ChannelId, ExtensionName, Frame, Grant, Kind, Limits, Reply, Request, Session, Transit,
    Welcome, Wire, WorkspaceInfo, PROTOCOL,
};

mod host;
mod producer;

use host::Workspace;

/// Why the hosted conversation ended early.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    Socket(String),
    Malformed(String),
    Refused(String),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Socket(detail) => write!(formatter, "socket: {detail}"),
            Self::Malformed(detail) => write!(formatter, "malformed: {detail}"),
            Self::Refused(detail) => write!(formatter, "refused: {detail}"),
        }
    }
}

impl std::error::Error for Fault {}

/// Runs the extension until it has described its interface, applying every
/// frame it sends to the widget tree.
///
/// # Errors
/// Returns why the conversation could not be completed.
pub fn host(widgets: &mut Widgets, tree: &mut Tree) -> Result<usize, Fault> {
    let (ours, theirs) = UnixStream::pair().map_err(|error| Fault::Socket(error.to_string()))?;
    let extension = std::thread::spawn(move || extension::serve(theirs, extension::Extension::new()));
    converse_with(ours, widgets, tree, extension)
}

/// Runs the whole component catalogue through the same socket path, so every
/// component in the library is shown to survive being described remotely
/// rather than only the ones the reference extension happens to use.
///
/// # Errors
/// Returns why the conversation could not be completed.
pub fn catalogue(widgets: &mut Widgets, tree: &mut Tree, filter: Option<String>) -> Result<usize, Fault> {
    let (ours, theirs) = UnixStream::pair().map_err(|error| Fault::Socket(error.to_string()))?;
    let producer = std::thread::spawn(move || producer::serve(theirs, filter.as_deref()));
    converse_with(ours, widgets, tree, producer)
}

/// Drives one producer to completion and applies everything it describes.
fn converse_with<T>(
    stream: UnixStream,
    widgets: &mut Widgets,
    tree: &mut Tree,
    producer: std::thread::JoinHandle<T>,
) -> Result<usize, Fault> {
    let ours = stream;

    let mut wire = Wire::new(ours);
    let mut session = Session::new(authority());
    greet(&mut wire)?;
    let mut applied = converse(&mut wire, &mut session, widgets, tree)?;
    applied += fill(&mut wire, widgets)?;

    // Dropping our end ends the producer's loop; it reports a clean close.
    drop(wire);
    let _ = producer.join();
    Ok(applied)
}

/// The grant the storybook offers: draw, and read containers. Nothing else.
fn authority() -> Authority {
    Authority::new(
        ExtensionName::new("containers").expect("a valid name"),
        Grant::new([Capability::ContainerRead, Capability::Interface]),
        Vec::new(),
    )
}

/// Sends the opening frame and reads the extension's reply, so the extension
/// knows its grant before it asks for anything.
fn greet(wire: &mut Wire<UnixStream>) -> Result<(), Fault> {
    let welcome = Welcome {
        protocol: PROTOCOL,
        host: env!("CARGO_PKG_VERSION").into(),
        workspace: "storybook".into(),
        extension: ExtensionName::new("containers").expect("a valid name"),
        granted: Grant::new([Capability::ContainerRead, Capability::Interface]),
        limits: Limits::default(),
    };
    let payload = serde_json::to_vec(&welcome).map_err(|error| Fault::Malformed(error.to_string()))?;
    wire.send(&Frame::control(Kind::Request, payload)).map_err(transit)?;
    wire.receive().map_err(transit)?;
    Ok(())
}

/// Answers the extension's calls until its interface is complete.
///
/// Both sides block on their socket, so the host has to know when to stop
/// listening. The interface is complete once the extension has both described
/// it and said how long its table is; waiting past that would deadlock, since
/// the extension is by then waiting on the host.
fn converse(
    wire: &mut Wire<UnixStream>,
    session: &mut Session,
    widgets: &mut Widgets,
    tree: &mut Tree,
) -> Result<usize, Fault> {
    let workspace = Workspace::new();
    let mut progress = Progress::default();
    for _ in 0..EXCHANGES {
        let frame = match wire.receive() {
            Ok(frame) => frame,
            Err(Transit::Closed) => break,
            Err(other) => return Err(transit(other)),
        };
        let Ok(request) = serde_json::from_slice::<Request>(&frame.payload) else {
            continue;
        };
        progress.note(&request);
        progress.applied += exchange(wire, session, widgets, tree, &workspace, &request)?;
        if progress.is_complete() {
            break;
        }
    }
    Ok(progress.applied)
}

/// How far the extension has got. A ceiling on exchanges bounds a misbehaving
/// extension; the completion check is what ends a well-behaved one.
#[derive(Default)]
struct Progress {
    applied: usize,
    drawn: bool,
    sized: bool,
}

impl Progress {
    fn note(&mut self, request: &Request) {
        match request {
            Request::InterfaceRender { .. } => self.drawn = true,
            Request::SourceResize { .. } => self.sized = true,
            _ => {}
        }
    }

    const fn is_complete(&self) -> bool {
        self.drawn && self.sized
    }
}

/// Calls the storybook answers before giving up on an extension that never
/// finishes describing itself.
const EXCHANGES: usize = 16;

/// Handles one call: dispatch it, apply anything it drew, and reply.
fn exchange(
    wire: &mut Wire<UnixStream>,
    session: &mut Session,
    widgets: &mut Widgets,
    tree: &mut Tree,
    workspace: &Workspace,
    request: &Request,
) -> Result<usize, Fault> {
    let reply = session
        .dispatch(request, &workspace.services())
        .map_err(|failure| Fault::Refused(format!("{failure:?}")))?;
    let applied = draw(session, widgets, tree)?;
    respond(wire, &reply)?;
    Ok(applied)
}

/// Applies whatever the extension drew since the last call.
fn draw(session: &mut Session, widgets: &mut Widgets, tree: &mut Tree) -> Result<usize, Fault> {
    let mut applied = 0;
    for frame in session.drain() {
        tree.apply(&frame, widgets)
            .map_err(|fault| Fault::Malformed(fault.to_string()))?;
        applied += frame.patches.len();
    }
    for mutation in session.drain_sources() {
        resize(widgets, &mutation);
    }
    Ok(applied)
}

/// Answers the tables' row requests from the extension.
///
/// A table asks for the windows it needs as it realizes rows, so the host has
/// to carry those requests to the extension and the answers back; without this
/// leg the interface is present but every row is a placeholder.
fn fill(wire: &mut Wire<UnixStream>, widgets: &mut Widgets) -> Result<usize, Fault> {
    let mut delivered = 0;
    for round in 0..ROUNDS {
        let requests = widgets.requests(round);
        if requests.is_empty() {
            break;
        }
        for request in &requests {
            deliver(wire, widgets, request)?;
            delivered += 1;
        }
    }
    Ok(delivered)
}

/// Asks for one window and gives the answer to the table that wanted it.
fn deliver(wire: &mut Wire<UnixStream>, widgets: &mut Widgets, request: &hl_gui::RowRequest) -> Result<(), Fault> {
    let payload = serde_json::to_vec(request).map_err(|error| Fault::Malformed(error.to_string()))?;
    wire.send(&Frame::new(ChannelId::new(3), Kind::Event, payload))
        .map_err(transit)?;
    let frame = wire.receive().map_err(transit)?;
    let window: hl_gui::RowWindow =
        serde_json::from_slice(&frame.payload).map_err(|error| Fault::Malformed(error.to_string()))?;
    if let Err(failure) = widgets.rows(&window) {
        eprintln!("[storybook] window rejected: {failure}");
    }
    Ok(())
}

/// Fetch rounds before the host stops asking, so a table over a source that
/// never satisfies it cannot spin.
const ROUNDS: u64 = 4;

/// Records a source's length so its table describes the whole listing.
fn resize(widgets: &mut Widgets, mutation: &hl_gui::SourceMutation) {
    let hl_gui::SourceMutation::Length { source, version, rows } = mutation else {
        return;
    };
    let Err(failure) = widgets.resize(*source, *version, *rows) else {
        return;
    };
    // A source no table is bound to is ordinary when only part of the
    // catalogue was asked for; anything else is worth saying out loud.
    if matches!(failure, hl_gui_gtk::Failure::Unbound(_)) {
        return;
    }
    eprintln!("[storybook] source rejected: {failure}");
}

fn respond(wire: &mut Wire<UnixStream>, reply: &Reply) -> Result<(), Fault> {
    let payload = serde_json::to_vec(reply).map_err(|error| Fault::Malformed(error.to_string()))?;
    wire.send(&Frame::new(ChannelId::new(1), Kind::Response, payload))
        .map_err(transit)
}

fn transit(transit: Transit) -> Fault {
    match transit {
        Transit::Closed => Fault::Socket("the extension closed the connection".into()),
        Transit::Malformed(malformed) => Fault::Malformed(malformed.to_string()),
        Transit::Io(detail) => Fault::Socket(detail),
    }
}

/// Containers the storybook pretends the workspace is running.
#[must_use]
pub fn containers() -> Vec<ContainerSummary> {
    [
        ("api", "husklet/api:1.4.2", "running"),
        ("worker", "husklet/worker:1.4.2", "running"),
        ("postgres", "postgres:16-alpine", "restarting"),
        ("redis", "redis:7-alpine", "exited"),
        ("migrate", "husklet/migrate:1.4.2", "created"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, image, state))| ContainerSummary {
        id: format!("c{index}"),
        name: name.into(),
        image: image.into(),
        state: state.into(),
        created: 1_700_000_000 + index as i64,
    })
    .collect()
}

/// The workspace description the extension is told about.
#[must_use]
pub fn workspace() -> WorkspaceInfo {
    WorkspaceInfo {
        name: "storybook".into(),
        architecture: "arm64".into(),
        image: "alpine:3.20".into(),
    }
}
