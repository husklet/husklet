//! One extension's conversation, from the opening frame to the last call.
//!
//! `hl-extension` owns the protocol and opens no socket; this is the half
//! that holds one. A conversation is the join between a connected
//! [`UnixStream`] and a [`Session`]: it reads frames, decodes them, dispatches
//! through the session against the real adapters, and writes back what the
//! session answered.
//!
//! Nothing here draws. An interface frame an extension sends is drained from
//! the session into a [`Queue`] the GUI thread collects, because this module
//! owns no toolkit and the surface belongs to the window. That split is what
//! lets the whole conversation be exercised over a socket pair with no window
//! open, which is exactly what the tests below do.

use std::io;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hl_extension::{
    codec, Authority, Channels, Compatibility, Emission, Failure, Frame, Hello, Kind, Limits, Outbox, Reply, Services,
    Session, Snapshot, Streams, Subscriptions, Topic, Transit, Welcome, Wire, PROTOCOL,
};

/// Interface work an extension has produced and the GUI has not collected yet.
#[derive(Debug, Default)]
pub struct Interface {
    /// Descriptions of what to draw, in the order the extension sent them.
    pub frames: Vec<hl_gui::Frame>,
    /// Changes to the windowed sources the extension's tables draw from.
    pub mutations: Vec<hl_gui::SourceMutation>,
}

impl Interface {
    /// Whether there is anything for the GUI to apply.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty() && self.mutations.is_empty()
    }
}

/// Where a conversation leaves interface work for the GUI thread.
///
/// A handle rather than a queue passed by value: the conversation runs on its
/// own thread and the window runs on the main one, so the two need the same
/// queue and neither may own it exclusively.
#[derive(Clone, Debug, Default)]
pub struct Queue {
    held: Arc<Mutex<Interface>>,
}

impl Queue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes everything waiting, leaving the queue empty.
    #[must_use]
    pub fn collect(&self) -> Interface {
        let mut held = self.hold();
        Interface {
            frames: std::mem::take(&mut held.frames),
            mutations: std::mem::take(&mut held.mutations),
        }
    }

    /// Whether the GUI has anything to collect.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hold().is_empty()
    }

    /// Adds what a session drained. Poisoning is recovered from rather than
    /// propagated: the queue is a list of drawing work, so a thread that
    /// panicked mid-deposit leaves it stale at worst, never unsound.
    fn deposit(&self, frames: Vec<hl_gui::Frame>, mutations: Vec<hl_gui::SourceMutation>) {
        let mut held = self.hold();
        held.frames.extend(frames);
        held.mutations.extend(mutations);
    }

    fn hold(&self) -> std::sync::MutexGuard<'_, Interface> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why a conversation ended before the peer hung up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// The socket failed.
    Socket(String),
    /// The peer's bytes were not what the protocol says they are.
    Malformed(String),
    /// The handshake did not produce an agreed version. `Compatibility::Unknown`
    /// is a peer that never spoke, which is a different event from one that
    /// spoke a version this host does not have.
    Handshake(Compatibility),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Socket(detail) => write!(formatter, "the extension socket failed: {detail}"),
            Self::Malformed(detail) => write!(formatter, "the extension sent {detail}"),
            Self::Handshake(outcome) => write!(formatter, "the handshake did not complete: {outcome}"),
        }
    }
}

impl std::error::Error for Fault {}

impl From<io::Error> for Fault {
    fn from(error: io::Error) -> Self {
        Self::Socket(error.to_string())
    }
}

/// Translates a transport outcome, keeping a malformed peer distinct from a
/// broken socket so a misbehaving extension is not reported as a host failure.
fn fault(transit: Transit) -> Fault {
    match transit {
        Transit::Closed => Fault::Socket("the extension closed the connection".to_owned()),
        Transit::Malformed(reason) => Fault::Malformed(reason.to_string()),
        Transit::Io(detail) => Fault::Socket(detail),
    }
}

/// The host end of one connected extension.
pub struct Conversation {
    wire: Wire<UnixStream>,
    /// A second descriptor for the same socket, kept because [`Wire`] owns the
    /// stream and the read deadline has to be set from outside it.
    control: UnixStream,
    session: Session,
    subscriptions: Subscriptions,
    streams: Streams,
    channels: Channels,
    outbox: Outbox,
    queue: Queue,
    workspace: String,
    settle: Duration,
}

impl Conversation {
    /// How long a connected peer has to complete the handshake.
    ///
    /// A process that connects and says nothing would otherwise hold the one
    /// conversation an extension gets for as long as it likes.
    pub const SETTLE: Duration = Duration::from_secs(5);

    /// Wraps an accepted connection for one extension.
    ///
    /// # Errors
    /// Returns the failure to duplicate the socket descriptor.
    pub fn new(
        stream: UnixStream,
        authority: Authority,
        workspace: impl Into<String>,
        queue: Queue,
    ) -> io::Result<Self> {
        let control = stream.try_clone()?;
        Ok(Self {
            wire: Wire::new(stream),
            control,
            session: Session::new(authority),
            subscriptions: Subscriptions::new(),
            streams: Streams::new(),
            channels: Channels::new(),
            outbox: Outbox::new(),
            queue,
            workspace: workspace.into(),
            settle: Self::SETTLE,
        })
    }

    /// Shortens or lengthens the handshake window.
    #[must_use]
    pub const fn settling(mut self, settle: Duration) -> Self {
        self.settle = settle;
        self
    }

    /// What this extension is allowed to do.
    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    /// The topics this extension follows and the channels they ride on.
    #[must_use]
    pub const fn subscriptions(&self) -> &Subscriptions {
        &self.subscriptions
    }

    /// The byte streams open to this extension.
    #[must_use]
    pub const fn streams(&self) -> &Streams {
        &self.streams
    }

    /// Speaks first, stating the grant, and reads the extension's reply.
    ///
    /// The host opens the conversation so an extension knows what it holds
    /// before it asks for anything. A version this host does not speak is
    /// refused with a reset naming both versions, so the extension can say
    /// which host it needs rather than only that it was disconnected.
    ///
    /// # Errors
    /// Returns `Fault::Handshake` when the peer declares another version or
    /// does not finish inside the settle window, and `Fault::Socket` or
    /// `Fault::Malformed` when the reply could not be read.
    pub fn greet(&mut self) -> Result<Hello, Fault> {
        self.welcome()?;
        let hello = self.hello()?;
        let outcome = Compatibility::of(hello.protocol);
        if outcome.is_compatible() {
            return Ok(hello);
        }
        self.reset(outcome);
        Err(Fault::Handshake(outcome))
    }

    /// Answers calls until the extension hangs up.
    ///
    /// # Errors
    /// Returns why the conversation ended, except a clean hangup, which is the
    /// ordinary end of a session and is reported as success.
    pub fn serve(&mut self, services: &Services<'_>) -> Result<(), Fault> {
        loop {
            match self.wire.receive() {
                Ok(frame) => self.exchange(&frame, services)?,
                Err(Transit::Closed) => return Ok(()),
                Err(other) => return Err(fault(other)),
            }
        }
    }

    /// Queues a listing for an extension that follows its topic.
    ///
    /// The channel is allocated on first use rather than at subscribe time, so
    /// a topic nothing is ever published on costs no channel.
    ///
    /// # Errors
    /// Returns `Fault::Malformed` when the listing cannot be encoded.
    pub fn publish(&mut self, snapshot: &Snapshot) -> Result<Emission, Fault> {
        let topic = snapshot.topic();
        if !self.session.may_emit(topic) {
            return Ok(Emission::Ignored);
        }
        let payload = snapshot
            .payload()
            .map_err(|coding| Fault::Malformed(coding.to_string()))?;
        self.route(topic)?;
        let emission = self
            .subscriptions
            .emit(topic, payload, &self.session, &mut self.channels, &mut self.outbox);
        self.flush()?;
        Ok(emission)
    }

    /// Sends the opening frame.
    fn welcome(&mut self) -> Result<(), Fault> {
        let welcome = Welcome {
            protocol: PROTOCOL,
            host: env!("CARGO_PKG_VERSION").to_owned(),
            workspace: self.workspace.clone(),
            extension: self.session.authority().extension().clone(),
            granted: self.session.authority().granted().clone(),
            limits: Limits::default(),
        };
        let frame = codec::welcome(&welcome).map_err(|coding| Fault::Malformed(coding.to_string()))?;
        self.wire.send(&frame).map_err(fault)
    }

    /// Reads the reply under a deadline, so an unfinished handshake ends the
    /// connection instead of holding it.
    fn hello(&mut self) -> Result<Hello, Fault> {
        self.control.set_read_timeout(Some(self.settle))?;
        let started = Instant::now();
        let received = self.wire.receive();
        self.control.set_read_timeout(None)?;
        let frame = received.map_err(|transit| self.unsettled(transit, started))?;
        codec::read_hello(&frame).map_err(|coding| Fault::Malformed(coding.to_string()))
    }

    /// Classifies a failed read of the reply.
    ///
    /// A peer that ran out its window, or hung up without speaking, declared no
    /// version at all: that is `Compatibility::Unknown` and must never be
    /// recorded as a version mismatch, which would blame an extension for a
    /// version it never named.
    fn unsettled(&self, transit: Transit, started: Instant) -> Fault {
        if matches!(transit, Transit::Closed) || started.elapsed() >= self.settle {
            return Fault::Handshake(Compatibility::Unknown);
        }
        fault(transit)
    }

    /// Tells the peer why it is being disconnected. A failure to send is
    /// swallowed: the connection is already ending, and the outcome the caller
    /// receives is the mismatch, not the courtesy that followed it.
    fn reset(&mut self, outcome: Compatibility) {
        let frame = Frame::control(Kind::Reset, outcome.to_string().into_bytes());
        let _ = self.wire.send(&frame);
    }

    /// Handles one frame from the peer.
    fn exchange(&mut self, frame: &Frame, services: &Services<'_>) -> Result<(), Fault> {
        let Some(answer) = self.answer(frame, services) else {
            return Ok(());
        };
        // Gather before answering: once the peer has its reply it may act on
        // it, and an effect the call produced must already be observable by
        // then rather than racing the window's next collection.
        self.gather();
        self.respond(&answer)?;
        self.flush()
    }

    /// The answer a frame deserves, or nothing when it asked no question.
    fn answer(&mut self, frame: &Frame, services: &Services<'_>) -> Option<Result<Reply, Failure>> {
        match frame.kind {
            Kind::Request => Some(self.call(frame, services)),
            Kind::Credit => {
                self.replenish(frame);
                None
            }
            _ => None,
        }
    }

    /// Decodes and dispatches one call. A frame this host cannot read as a call
    /// is refused rather than ending the conversation, because a peer waiting
    /// on a reply learns more from the refusal than from a closed socket.
    fn call(&mut self, frame: &Frame, services: &Services<'_>) -> Result<Reply, Failure> {
        let request = codec::read_request(frame).map_err(|coding| Failure::Unsupported {
            call: coding.to_string(),
        })?;
        self.session.dispatch(&request, services)
    }

    /// Returns the credit a peer released as it consumed frames. A payload that
    /// is not a count, or a channel that has since closed, is ignored: stale
    /// credit is ordinary on a channel the host already tore down.
    fn replenish(&mut self, frame: &Frame) {
        let Ok(frames) = serde_json::from_slice::<u32>(&frame.payload) else {
            return;
        };
        let _ = self.channels.replenish(frame.channel, frames);
    }

    /// Writes the answer to one call.
    fn respond(&mut self, answer: &Result<Reply, Failure>) -> Result<(), Fault> {
        let frame = match answer {
            Ok(reply) => codec::reply(reply),
            Err(failure) => codec::failure(failure),
        };
        let frame = frame.map_err(|coding| Fault::Malformed(coding.to_string()))?;
        self.wire.send(&frame).map_err(fault)
    }

    /// Moves what the session collected into the queue the GUI reads.
    fn gather(&mut self) {
        self.queue.deposit(self.session.drain(), self.session.drain_sources());
    }

    /// Allocates the channel a topic is delivered on.
    fn route(&mut self, topic: Topic) -> Result<(), Fault> {
        self.subscriptions
            .open(topic, &mut self.channels)
            .map(|_| ())
            .map_err(|refusal| Fault::Socket(refusal.to_string()))
    }

    /// Writes what the outbox released.
    ///
    /// Credit is honoured by the outbox, which hands over only what a channel
    /// has reserved, so this sends everything it is given and never rations
    /// again on its own.
    fn flush(&mut self) -> Result<(), Fault> {
        for topic in self.session.topics() {
            self.carry(topic)?;
        }
        Ok(())
    }

    /// Writes one topic's released messages.
    fn carry(&mut self, topic: Topic) -> Result<(), Fault> {
        let Some(channel) = self.subscriptions.channel(topic) else {
            return Ok(());
        };
        for message in self.outbox.drain(channel) {
            let frame = Frame::new(channel, Kind::Event, message.payload);
            self.wire.send(&frame).map_err(fault)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use hl_extension::port::{
        ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
        PaneSummary, TabSummary, TerminalSurface, WorkspaceFiles,
    };
    use hl_extension::{
        codec, Authority, Capability, ExtensionName, Failure, Frame, Grant, Hello, Kind, RelativePath, Reply, Request,
        Services, Transit, Wire, WorkspaceInfo, PROTOCOL,
    };

    use super::{Compatibility, Conversation, Fault, Queue};

    /// What the adapters were actually asked for, so a refusal that still
    /// reached a service would be visible rather than silent.
    #[derive(Debug, Default)]
    struct Ledger {
        reached: Mutex<Vec<&'static str>>,
    }

    impl Ledger {
        fn note(&self, what: &'static str) {
            self.reached.lock().expect("ledger").push(what);
        }

        fn reached(&self) -> Vec<&'static str> {
            self.reached.lock().expect("ledger").clone()
        }
    }

    /// In-memory adapters: no container runtime and no window.
    struct Host {
        ledger: Arc<Ledger>,
    }

    impl ContainerInventory for Host {
        fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
            self.ledger.note("containers.list");
            Ok(vec![ContainerSummary {
                id: "c1".to_owned(),
                name: "api".to_owned(),
                image: "husklet/api:1".to_owned(),
                state: "running".to_owned(),
                created: 0,
            }])
        }

        fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
            self.ledger.note("containers.inspect");
            Err(HostError::Absent(id.to_owned()))
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
                id: "i1".to_owned(),
                reference: reference.to_owned(),
                size: 1,
                created: 0,
            })
        }
    }

    impl TerminalSurface for Host {
        fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
            self.ledger.note("terminal.tabs");
            Ok(vec![TabSummary {
                id: "t1".to_owned(),
                title: "shell".to_owned(),
                panes: vec![PaneSummary {
                    slot: "s1".to_owned(),
                    working_directory: None,
                    command: None,
                }],
            }])
        }

        fn open_tab(&self, title: &str) -> Result<String, HostError> {
            self.ledger.note("terminal.open_tab");
            Ok(format!("tab-{title}"))
        }

        fn split(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
            self.ledger.note("terminal.split");
            Ok("s2".to_owned())
        }

        fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
            self.ledger.note("terminal.spawn");
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

        fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
            self.ledger.note("files.write");
            Ok(())
        }
    }

    fn services(host: &Host) -> Services<'_> {
        Services {
            workspace: WorkspaceInfo {
                name: "dev".to_owned(),
                architecture: "arm64".to_owned(),
                image: "alpine:3.20".to_owned(),
            },
            containers: host,
            control: host,
            images: host,
            terminal: host,
            files: host,
        }
    }

    /// The grant every test starts from: read containers, and draw.
    fn authority() -> Authority {
        Authority::new(
            ExtensionName::new("sample").expect("name"),
            Grant::new([Capability::ContainerRead, Capability::Interface]),
            Vec::new(),
        )
    }

    /// Runs the host end on its own thread, as the listener does, and answers
    /// on the returned stream as an extension would.
    fn host(settle: Duration, queue: Queue, ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let mut conversation = Conversation::new(ours, authority(), "dev", queue)?.settling(settle);
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        (theirs, served)
    }

    /// Reads the welcome and answers it with a version.
    fn shake(wire: &mut Wire<UnixStream>, protocol: u32) {
        let frame = wire.receive().expect("welcome");
        codec::read_welcome(&frame).expect("a welcome");
        let hello = Hello {
            protocol,
            name: ExtensionName::new("sample").expect("name"),
            features: Vec::new(),
        };
        wire.send(&codec::hello(&hello).expect("encoded")).expect("sent");
    }

    fn ask(wire: &mut Wire<UnixStream>, request: &Request) -> Frame {
        wire.send(&codec::request(request).expect("encoded")).expect("sent");
        wire.receive().expect("an answer")
    }

    #[test]
    fn a_greeted_extension_has_its_call_answered() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);

        shake(&mut wire, PROTOCOL);
        let answer = ask(&mut wire, &Request::ContainerList);

        assert_eq!(
            codec::read_reply(&answer).expect("a reply"),
            Reply::Containers(vec![ContainerSummary {
                id: "c1".to_owned(),
                name: "api".to_owned(),
                image: "husklet/api:1".to_owned(),
                state: "running".to_owned(),
                created: 0,
            }])
        );
        assert_eq!(ledger.reached(), vec!["containers.list"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()), "a hangup is not a fault");
    }

    #[test]
    fn the_welcome_states_the_grant_before_anything_is_asked() {
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);

        let frame = wire.receive().expect("welcome");

        let welcome = codec::read_welcome(&frame).expect("a welcome");
        assert_eq!(welcome.protocol, PROTOCOL);
        assert!(welcome.granted.holds(Capability::ContainerRead));
        assert!(!welcome.granted.holds(Capability::ContainerControl));
        drop(wire);
        let _ = served.join().expect("joined");
    }

    #[test]
    fn another_protocol_is_refused_with_both_versions_named() {
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);

        shake(&mut wire, PROTOCOL + 1);

        let reset = wire.receive().expect("a reset");
        assert_eq!(reset.kind, Kind::Reset);
        let message = String::from_utf8(reset.payload).expect("text");
        assert!(message.contains(&(PROTOCOL + 1).to_string()), "{message}");
        assert!(message.contains(&PROTOCOL.to_string()), "{message}");
        let fault = served.join().expect("joined").expect_err("refused");
        assert_eq!(
            fault,
            Fault::Handshake(Compatibility::Mismatched {
                declared: PROTOCOL + 1,
                supported: PROTOCOL,
            })
        );
    }

    #[test]
    fn a_peer_that_never_speaks_is_dropped_without_being_blamed_for_a_version() {
        let (theirs, served) = host(Duration::from_millis(150), Queue::new(), Arc::new(Ledger::default()));

        // Connected, welcomed, and silent: the window is the only thing that
        // ends this.
        let fault = served.join().expect("joined").expect_err("dropped");

        assert_eq!(fault, Fault::Handshake(Compatibility::Unknown));
        assert!(
            !matches!(fault, Fault::Handshake(Compatibility::Mismatched { .. })),
            "silence is not a version this host can disagree with"
        );
        assert!(fault.to_string().contains("not yet declared"), "{fault}");
        drop(theirs);
    }

    #[test]
    fn an_interface_frame_is_collected_for_the_window_rather_than_applied() {
        let queue = Queue::new();
        let (theirs, served) = host(Duration::from_secs(5), queue.clone(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        ask(
            &mut wire,
            &Request::InterfaceOpenTab {
                title: "Sample".to_owned(),
            },
        );
        let drawn = ask(
            &mut wire,
            &Request::InterfaceRender {
                frame: hl_gui::Frame::new(1),
            },
        );

        assert_eq!(codec::read_reply(&drawn).expect("a reply"), Reply::Done);
        let collected = queue.collect();
        assert_eq!(collected.frames.len(), 1, "the frame is held for the window");
        assert_eq!(collected.frames[0].sequence, 1);
        assert!(queue.is_empty(), "collecting empties the queue");
        drop(wire);
        let _ = served.join().expect("joined");
    }

    #[test]
    fn an_ungranted_call_is_refused_and_reaches_no_adapter() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(&mut wire, &Request::ContainerStop { id: "c1".to_owned() });

        assert!(codec::is_failure(&answer), "a refusal is reported as one");
        let Failure::Denied { capability, .. } = codec::read_failure(&answer).expect("a failure") else {
            panic!("an ungranted call is a denial");
        };
        assert_eq!(capability, Capability::ContainerControl.as_str());
        assert!(ledger.reached().is_empty(), "nothing may be reached before the check");
        drop(wire);
        let _ = served.join().expect("joined");
    }

    #[test]
    fn a_frame_that_is_not_a_call_is_refused_without_ending_the_conversation() {
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        wire.send(&Frame::new(codec::CALLS, Kind::Request, b"not a call".to_vec()))
            .expect("sent");
        let refused = wire.receive().expect("an answer");
        let answered = ask(&mut wire, &Request::ContainerList);

        assert!(codec::is_failure(&refused));
        assert!(codec::read_reply(&answered).is_ok(), "the conversation survives it");
        drop(wire);
        let _ = served.join().expect("joined");
    }

    #[test]
    fn a_closed_socket_ends_the_conversation_rather_than_faulting() {
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        drop(wire);

        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn a_transport_failure_is_not_reported_as_a_malformed_peer() {
        assert_eq!(
            super::fault(Transit::Io("broken pipe".to_owned())),
            Fault::Socket("broken pipe".to_owned())
        );
    }
}
