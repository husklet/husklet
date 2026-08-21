//! Hosts the reference extension over a real socket and renders what it sends.
//!
//! This is the end-to-end path in miniature: an extension in another thread,
//! speaking the protocol across a socket pair, describing an interface the
//! toolkit renders. Nothing about the interface is known here in advance.

#[cfg(unix)]
use std::io::{Read, Write};

#[cfg(unix)]
use hl_extension::port::ContainerSummary;
#[cfg(unix)]
use hl_extension::WorkspaceInfo;
#[cfg(unix)]
use hl_extension::{
    Authority, Capability, ChannelId, ExtensionName, Frame, Grant, Kind, Limits, Reply, Request, Session, Transit,
    Welcome, Wire, PROTOCOL,
};
#[cfg(unix)]
use hl_gui::Renderer;
use hl_gui::Tree;
use hl_gui_gtk::Surface as Widgets;

/* The conversation below is a demonstration over a Unix-domain socket, so its whole call graph --
 * the two submodules that produce frames included -- is reachable only from the three entry points
 * that build one. Declaring the submodules under the same gate as their callers is what keeps this
 * module's contents and its consumers the same width in both configurations; a portable module with
 * no non-Unix consumer would be dead code there, which `-D warnings` reports and nobody reads. */
#[cfg(unix)]
mod host;
#[cfg(unix)]
mod process;
#[cfg(unix)]
mod producer;

#[cfg(unix)]
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
#[cfg(unix)]
pub fn host(widgets: &mut Widgets, tree: &mut Tree) -> Result<usize, Fault> {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(|error| Fault::Socket(error.to_string()))?;
    let extension = std::thread::spawn(move || extension::serve(theirs, extension::Extension::new()));
    converse_with(ours, widgets, tree, extension)
}

/// # Errors
/// Always: see [`NO_SOCKET_PAIR`].
#[cfg(not(unix))]
pub fn host(_: &mut Widgets, _: &mut Tree) -> Result<usize, Fault> {
    Err(Fault::Socket(NO_SOCKET_PAIR.into()))
}

/// Runs the whole component catalogue through the same socket path, so every
/// component in the library is shown to survive being described remotely
/// rather than only the ones the reference extension happens to use.
///
/// # Errors
/// Returns why the conversation could not be completed.
#[cfg(unix)]
pub fn catalogue(widgets: &mut Widgets, tree: &mut Tree, filter: Option<String>) -> Result<usize, Fault> {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(|error| Fault::Socket(error.to_string()))?;
    let producer = std::thread::spawn(move || producer::serve(theirs, filter.as_deref()));
    converse_with(ours, widgets, tree, producer)
}

/// # Errors
/// Always: see [`NO_SOCKET_PAIR`].
#[cfg(not(unix))]
pub fn catalogue(_: &mut Widgets, _: &mut Tree, _: Option<String>) -> Result<usize, Fault> {
    Err(Fault::Socket(NO_SOCKET_PAIR.into()))
}

/// Hosts an extension running as a real process, rendering whatever it draws.
///
/// The in-process modes prove the protocol talks to itself. This proves the
/// product: a command is started knowing only a socket path, exactly as a
/// sidecar container is, and its interface is rendered here.
///
/// # Errors
/// Returns why the extension could not be started or did not finish drawing.
#[cfg(unix)]
pub fn spawned(widgets: &mut Widgets, tree: &mut Tree, command: &str) -> Result<usize, Fault> {
    let guest = process::Guest::invite(command)?;
    let stream = guest.accept()?;

    let mut wire = Wire::new(stream);
    let mut session = Session::new(authority());
    greet(&mut wire)?;
    let mut applied = converse(&mut wire, &mut session, widgets, tree, 0)?;
    applied += fill(&mut wire, widgets)?;
    // The guest is stopped when it drops, which also unlinks the socket.
    Ok(applied)
}

/// # Errors
/// Always: see [`NO_LISTENER`].
#[cfg(not(unix))]
pub fn spawned(_: &mut Widgets, _: &mut Tree, _: &str) -> Result<usize, Fault> {
    Err(Fault::Socket(NO_LISTENER.into()))
}

/// Why the two in-process modes refuse off Unix.
///
/// Both hand one end of a connected stream pair to a producer on another thread and keep the
/// other. `std::os::unix::net::UnixStream::pair` is the only thing in the standard library that
/// makes one; Windows has `AF_UNIX` sockets since 1803 but `std` exposes no binding for them, and
/// the alternatives -- a named pipe, or a loopback listener -- are a different object with a
/// different lifetime, not a spelling change. Refusing here rather than substituting one keeps the
/// storybook honest about which host it demonstrated the protocol on.
#[cfg(not(unix))]
const NO_SOCKET_PAIR: &str =
    "the storybook's in-process extension modes need a connected socket pair, which the standard \
     library provides only on Unix";

/// Why the spawned mode refuses off Unix.
///
/// It is the product path, and the product contract is a filesystem path in
/// `HUSKLET_EXTENSION_SOCKET` that an extension in another process connects to -- exactly what a
/// sidecar container receives. Windows would deliver that rendezvous as a named pipe, which is a
/// different contract for the extension as well as for the host, so this refuses instead of
/// quietly demonstrating something the product does not do.
#[cfg(not(unix))]
const NO_LISTENER: &str =
    "the storybook's spawned extension mode needs a filesystem-path socket to invite a process to, \
     which the standard library provides only on Unix";

/// Drives one producer to completion and applies everything it describes.
#[cfg(unix)]
fn converse_with<S: Read + Write, T>(
    stream: S,
    widgets: &mut Widgets,
    tree: &mut Tree,
    producer: std::thread::JoinHandle<T>,
) -> Result<usize, Fault> {
    let ours = stream;

    let mut wire = Wire::new(ours);
    let mut session = Session::new(authority());
    greet(&mut wire)?;
    let mut applied = converse(&mut wire, &mut session, widgets, tree, crate::sources().len())?;
    applied += fill(&mut wire, widgets)?;

    // Dropping our end ends the producer's loop; it reports a clean close.
    drop(wire);
    let _ = producer.join();
    Ok(applied)
}

#[cfg(unix)]
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
#[cfg(unix)]
fn greet<S: Read + Write>(wire: &mut Wire<S>) -> Result<(), Fault> {
    let welcome = Welcome {
        protocol: PROTOCOL,
        host: env!("CARGO_PKG_VERSION").into(),
        workspace: "storybook".into(),
        peer: ExtensionName::new("containers").expect("a valid name"),
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
#[cfg(unix)]
fn converse<S: Read + Write>(
    wire: &mut Wire<S>,
    session: &mut Session,
    widgets: &mut Widgets,
    tree: &mut Tree,
    sources: usize,
) -> Result<usize, Fault> {
    let workspace = Workspace::new();
    let mut progress = Progress {
        expected: sources,
        ..Progress::default()
    };
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
#[cfg(unix)]
#[derive(Default)]
struct Progress {
    applied: usize,
    drawn: bool,
    sized: usize,
    /// How many sources this producer was expected to describe. An extension
    /// that draws no table describes none, and waiting for one would hang.
    expected: usize,
}

#[cfg(unix)]
impl Progress {
    fn note(&mut self, request: &Request) {
        match request {
            Request::InterfaceRender { .. } => self.drawn = true,
            Request::SourceResize { .. } => self.sized += 1,
            _ => {}
        }
    }

    /// Complete once the interface is described and every source it draws
    /// from has a length. Stopping at the first would leave later tables
    /// empty, since a table with no length has nothing to ask for.
    const fn is_complete(&self) -> bool {
        self.drawn && self.sized >= self.expected
    }
}

/// Calls the storybook answers before giving up on an extension that never
/// finishes describing itself.
#[cfg(unix)]
const EXCHANGES: usize = 16;

/// Handles one call: dispatch it, apply anything it drew, and reply.
#[cfg(unix)]
fn exchange<S: Read + Write>(
    wire: &mut Wire<S>,
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
#[cfg(unix)]
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
#[cfg(unix)]
fn fill<S: Read + Write>(wire: &mut Wire<S>, widgets: &mut Widgets) -> Result<usize, Fault> {
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
#[cfg(unix)]
fn deliver<S: Read + Write>(
    wire: &mut Wire<S>,
    widgets: &mut Widgets,
    request: &hl_gui::RowRequest,
) -> Result<(), Fault> {
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
#[cfg(unix)]
const ROUNDS: u64 = 4;

/// Records a source's length so its table describes the whole listing.
#[cfg(unix)]
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

#[cfg(unix)]
fn respond<S: Read + Write>(wire: &mut Wire<S>, reply: &Reply) -> Result<(), Fault> {
    let payload = serde_json::to_vec(reply).map_err(|error| Fault::Malformed(error.to_string()))?;
    wire.send(&Frame::new(ChannelId::new(1), Kind::Response, payload))
        .map_err(transit)
}

#[cfg(unix)]
fn transit(transit: Transit) -> Fault {
    match transit {
        Transit::Closed => Fault::Socket("the extension closed the connection".into()),
        Transit::Malformed(malformed) => Fault::Malformed(malformed.to_string()),
        Transit::Io(detail) => Fault::Socket(detail),
    }
}

#[cfg(unix)]
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

#[cfg(unix)]
/// The workspace description the extension is told about.
#[must_use]
pub fn workspace() -> WorkspaceInfo {
    WorkspaceInfo {
        name: "storybook".into(),
        architecture: "arm64".into(),
        image: "alpine:3.20".into(),
    }
}
