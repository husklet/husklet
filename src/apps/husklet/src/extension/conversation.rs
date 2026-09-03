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

use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hl_extension::{
    Authority, Channels, Compatibility, Emission, Failure, Frame, Hello, Kind, Limits, Outbox, PROTOCOL, PaneChange,
    PaneChangeKind, Permission, Reply, Services, Session, Snapshot, Streams, Subscriptions, SurfaceFrame,
    SurfaceMutation, Topic, Transit, Welcome, Wire, codec,
};

/// Interface work an extension has produced and the GUI has not collected yet.
#[derive(Debug, Default)]
pub struct Interface {
    /// Descriptions of what to draw, in the order the extension sent them.
    pub frames: Vec<SurfaceFrame>,
    /// Changes to the windowed sources the extension's tables draw from.
    pub mutations: Vec<SurfaceMutation>,
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
    /// Maximum interface operations one extension may leave behind the GUI.
    pub const LIMIT: usize = 128;
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
    fn deposit(&self, frames: Vec<SurfaceFrame>, mutations: Vec<SurfaceMutation>) -> Result<(), Fault> {
        let mut held = self.hold();
        if mutations.iter().any(|mutation| {
            matches!(&mutation.mutation, hl_gui::SourceMutation::Window(window) if !window.text_is_bounded())
        }) {
            return Err(Fault::Malformed("a row window exceeded the text payload limit".into()));
        }
        let cost = |frames: &[SurfaceFrame], mutations: &[SurfaceMutation]| {
            frames
                .iter()
                // Empty frames still consume sequencing and a GTK commit.
                .fold(0usize, |total, frame| {
                    total.saturating_add(frame.frame.patches.len().max(1))
                })
                .saturating_add(mutations.iter().fold(0usize, |total, mutation| {
                    let work = match &mutation.mutation {
                        hl_gui::SourceMutation::Open { columns, .. } => columns.len().max(1),
                        hl_gui::SourceMutation::Window(window) => window.rows.len().max(1),
                        _ => 1,
                    };
                    total.saturating_add(work)
                }))
        };
        let incoming = cost(&frames, &mutations);
        let occupied = cost(&held.frames, &held.mutations);
        if incoming > Self::LIMIT.saturating_sub(occupied) {
            return Err(Fault::Malformed(format!(
                "more than {} interface operations without letting the window catch up",
                Self::LIMIT
            )));
        }
        held.frames.extend(frames);
        held.mutations.extend(mutations);
        Ok(())
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
        Transit::Pending => Fault::Socket("the extension connection unexpectedly had no frame ready".to_owned()),
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
    observed: std::collections::BTreeMap<Topic, Snapshot>,
    extension_events: Option<super::management_events::ExtensionEvents>,
    pane_observed: std::collections::BTreeMap<String, (PaneChangeKind, u64, u64, u64)>,
    pane_generation: u64,
    pane_next: Instant,
    pane_cursor: usize,
    workspace_lifecycle_revision: Option<u64>,
    events: Option<super::host::Events>,
    voice: Option<super::host::Voice>,
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
            observed: std::collections::BTreeMap::new(),
            extension_events: None,
            pane_observed: std::collections::BTreeMap::new(),
            pane_generation: 0,
            pane_next: Instant::now(),
            pane_cursor: 0,
            workspace_lifecycle_revision: None,
            events: None,
            voice: None,
        })
    }

    pub(crate) fn with_events(&mut self, events: super::host::Events) {
        self.events = Some(events);
    }

    pub(crate) fn with_voice(&mut self, voice: super::host::Voice) {
        self.voice = Some(voice);
    }

    /// Composes the native producer now; the protocol adapter drains it once
    /// the Extensions snapshot variant is available.
    pub(crate) fn with_extension_events(&mut self, events: super::management_events::ExtensionEvents) {
        self.extension_events = Some(events);
    }

    pub(crate) fn drain_extension_events(&self) -> Option<super::management_events::ExtensionEventBatch> {
        self.extension_events
            .as_ref()
            .and_then(super::management_events::ExtensionEvents::drain)
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
        const OBSERVE: Duration = Duration::from_millis(250);
        self.control.set_read_timeout(Some(OBSERVE))?;
        loop {
            match self.wire.receive() {
                Ok(frame) => self.exchange(&frame, services)?,
                Err(Transit::Pending) => self.observe(services)?,
                Err(Transit::Closed) => return Ok(()),
                Err(other) => return Err(fault(other)),
            }
        }
    }

    /// Publishes changed full listings for topics backed by real production ports.
    ///
    /// Failed reads produce no event: publishing an empty list would falsely
    /// report that resources disappeared. `publish` retains the existing
    /// capability check, channel credit, and latest-snapshot coalescing.
    fn observe(&mut self, services: &Services<'_>) -> Result<(), Fault> {
        self.flush_interactions()?;
        let mut snapshots = Vec::new();
        if self.session.may_emit(Topic::Containers) {
            if let Ok(containers) = services.containers.list() {
                snapshots.push(Snapshot::Containers(containers));
            }
        }
        if self.session.may_emit(Topic::Executions) {
            if let Ok(executions) = services.containers.executions() {
                snapshots.push(Snapshot::Executions(executions));
            }
        }
        if self.session.may_emit(Topic::Images) {
            if let Ok(images) = services.images.list() {
                snapshots.push(Snapshot::Images(images));
            }
        }
        if self.session.may_emit(Topic::ImagePulls) {
            snapshots.extend(services.images.pull_changes().into_iter().map(Snapshot::ImagePulls));
        }
        if self.session.may_emit(Topic::Volumes) {
            if let Ok(volumes) = services.volumes.list() {
                snapshots.push(Snapshot::Volumes(volumes));
            }
        }
        if self.session.may_emit(Topic::Networks) {
            if let Ok(networks) = services.networks.list() {
                snapshots.push(Snapshot::Networks(networks));
            }
        }
        if self.session.may_emit(Topic::Terminal) {
            if let Ok(tabs) = services.terminal.tabs() {
                snapshots.push(Snapshot::Terminal(tabs));
            }
        }
        if self.session.may_emit(Topic::Extensions) {
            if let Ok(extensions) = services.extensions.list() {
                snapshots.push(Snapshot::Extensions(extensions));
            }
        }
        if self.session.may_emit(Topic::ExtensionAcquisitions) {
            if let Some(batch) = self.drain_extension_events() {
                for (index, invalidation) in batch.acquisitions.into_iter().enumerate() {
                    snapshots.push(Snapshot::ExtensionAcquisitions(
                        hl_extension::ExtensionAcquisitionChange {
                            job: invalidation.job,
                            revision: invalidation.snapshot.revision,
                            state: invalidation.snapshot.state.wire_state().into(),
                            // The native source coalesces every job to its latest
                            // revision. A capacity eviction is visible on the
                            // first surviving invalidation rather than hidden.
                            coalesced: if index == 0 { batch.dropped } else { 0 },
                        },
                    ));
                }
            }
        }
        if self.session.may_emit(Topic::WorkspaceEvents) {
            if let Some(batch) = self.events.as_ref().and_then(super::host::Events::drain) {
                snapshots.push(Snapshot::WorkspaceEvents(batch));
            }
        }
        if self.session.may_emit(Topic::WorkspaceLifecycle) {
            if let Some(revision) = self.workspace_lifecycle_revision {
                if let Ok(changes) = services.workspace_control.lifecycle_since(revision) {
                    for change in changes {
                        self.workspace_lifecycle_revision = Some(change.revision);
                        snapshots.push(Snapshot::WorkspaceLifecycle(change));
                    }
                }
            }
        }
        for snapshot in snapshots {
            let topic = snapshot.topic();
            if topic != Topic::WorkspaceEvents
                && topic != Topic::WorkspaceLifecycle
                && self.observed.get(&topic) == Some(&snapshot)
            {
                continue;
            }
            self.publish(&snapshot)?;
            if topic != Topic::WorkspaceEvents && topic != Topic::WorkspaceLifecycle {
                self.observed.insert(topic, snapshot);
            }
        }
        self.observe_panes(services)?;
        Ok(())
    }

    fn flush_interactions(&mut self) -> Result<(), Fault> {
        let frames = self.voice.as_ref().map_or_else(Vec::new, super::host::Voice::drain);
        for frame in frames {
            self.wire.send(&frame).map_err(fault)?;
        }
        Ok(())
    }

    /// Detects pane invalidations without ever putting pane contents on the
    /// event channel. This runs on the conversation worker after its timed
    /// receive, never from a GTK callback, and caps work to the protocol's
    /// semantic node budget worth of panes.
    fn observe_panes(&mut self, services: &Services<'_>) -> Result<(), Fault> {
        if !self.session.may_emit(Topic::PaneChanges) {
            return Ok(());
        }
        let Some(channel) = self.subscriptions.channel(Topic::PaneChanges) else {
            return Ok(());
        };
        // A stalled consumer cannot induce GTK work. Existing queued metadata
        // remains the invalidation until the client returns credit.
        if self.channels.credit(channel).unwrap_or(0) == 0 || Instant::now() < self.pane_next {
            return Ok(());
        }
        self.pane_next = Instant::now() + Duration::from_secs(1);
        let Ok(tabs) = services.terminal.tabs() else {
            return Ok(());
        };
        const PANE_SCAN_LIMIT: usize = 32;
        let topology = services.terminal.topology().ok().map(|topology| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_vec(&topology).unwrap_or_default().hash(&mut hash);
            hash.finish()
        });
        let panes: Vec<_> = tabs
            .into_iter()
            .flat_map(|tab| tab.panes)
            .take(256)
            .map(|pane| hl_extension::InspectablePane {
                slot: pane.slot,
                generation: 0,
                revision: 0,
                kind: match pane.occupant {
                    hl_extension::port::Occupant::Terminal => hl_extension::PaneKind::Terminal,
                    hl_extension::port::Occupant::Surface if pane.provider.is_some() => hl_extension::PaneKind::Surface,
                    hl_extension::port::Occupant::Surface => hl_extension::PaneKind::Native,
                },
                provider: pane.provider,
                tab: None,
                title: None,
                focused: false,
            })
            .collect();
        let live: std::collections::BTreeSet<_> = panes.iter().map(|pane| pane.slot.clone()).collect();
        let count = panes.len();
        let start = self.pane_cursor.min(count);
        self.pane_cursor = if count == 0 {
            0
        } else {
            (start + PANE_SCAN_LIMIT) % count
        };
        let mut changed = Vec::new();
        for pane in panes.into_iter().cycle().skip(start).take(PANE_SCAN_LIMIT.min(count)) {
            let (kind, revision, generation, pane_changed) = self.pane_state(services, &pane, topology);
            if pane_changed {
                changed.push((pane.slot.clone(), kind, revision, generation));
            }
        }
        // Removed panes also invalidate topology; retain only a bounded stable
        // identity and no former contents.
        for (slot, (kind, revision, _, _)) in &self.pane_observed {
            if !live.contains(slot) {
                self.pane_generation = self.pane_generation.saturating_add(1);
                changed.push((slot.clone(), *kind, *revision, self.pane_generation));
            }
        }
        self.pane_observed.retain(|slot, _| live.contains(slot));
        for (slot, kind, revision, generation) in changed.into_iter().take(PANE_SCAN_LIMIT) {
            self.publish(&Snapshot::PaneChanges(PaneChange {
                slot,
                kind,
                revision,
                generation,
                coalesced: 0,
            }))?;
        }
        Ok(())
    }

    fn pane_state(
        &mut self,
        services: &Services<'_>,
        pane: &hl_extension::InspectablePane,
        topology: Option<u64>,
    ) -> (PaneChangeKind, u64, u64, bool) {
        const TEXT_LINE_LIMIT: usize = 200;
        let kind = match pane.kind {
            hl_extension::PaneKind::Terminal => PaneChangeKind::Terminal,
            hl_extension::PaneKind::Surface => PaneChangeKind::Surface,
            hl_extension::PaneKind::Native => PaneChangeKind::Native,
        };
        let revision = services.terminal.semantics(&pane.slot).map_or(0, |tree| tree.revision);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        pane.slot.hash(&mut hash);
        pane.kind.hash(&mut hash);
        pane.provider.hash(&mut hash);
        topology.hash(&mut hash);
        if kind == PaneChangeKind::Terminal {
            if let Ok(mut text) = services.terminal.read(&pane.slot, TEXT_LINE_LIMIT) {
                text.generation = 0;
                text.revision = 0;
                serde_json::to_vec(&text).unwrap_or_default().hash(&mut hash);
            }
        }
        let fingerprint = hash.finish();
        let changed = self
            .pane_observed
            .get(&pane.slot)
            .is_none_or(|(_, old_revision, old_fingerprint, _)| {
                *old_revision != revision || *old_fingerprint != fingerprint
            });
        if changed {
            self.pane_generation = self.pane_generation.saturating_add(1);
        }
        let generation = if changed {
            self.pane_generation
        } else {
            self.pane_observed.get(&pane.slot).map_or(0, |state| state.3)
        };
        self.pane_observed
            .insert(pane.slot.clone(), (kind, revision, fingerprint, generation));
        (kind, revision, generation, changed)
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
        if emission == Emission::Queued {
            self.flush()?;
        }
        Ok(emission)
    }

    /// Sends the opening frame.
    fn welcome(&mut self) -> Result<(), Fault> {
        let welcome = Welcome {
            protocol: PROTOCOL,
            host: env!("CARGO_PKG_VERSION").to_owned(),
            workspace: self.workspace.clone(),
            peer: self.session.authority().peer().clone(),
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
        self.flush_interactions()?;
        if frame.kind == Kind::Credit {
            if let Some(topic) = self.replenish(frame) {
                self.carry(topic)?;
            }
            return Ok(());
        }
        let Some(answer) = self.answer(frame, services) else {
            return Ok(());
        };
        // Gather before answering: once the peer has its reply it may act on
        // it, and an effect the call produced must already be observable by
        // then rather than racing the window's next collection.
        self.gather()?;
        self.respond(&answer)?;
        self.flush()
    }

    /// The answer a frame deserves, or nothing when it asked no question.
    fn answer(&mut self, frame: &Frame, services: &Services<'_>) -> Option<Result<Reply, Failure>> {
        match frame.kind {
            Kind::Request => Some(self.call(frame, services)),
            Kind::Credit => None,
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
        let mut answer = self.session.dispatch(&request, services);
        if let Ok(reply) = &mut answer {
            self.attach_pane_cursors(reply, services);
        }
        if answer.is_ok() {
            match &request {
                hl_extension::Request::EventSubscribe {
                    topic: Topic::WorkspaceLifecycle,
                } => {
                    self.workspace_lifecycle_revision = Some(services.workspace_control.lifecycle_revision());
                }
                hl_extension::Request::EventUnsubscribe {
                    topic: Topic::WorkspaceLifecycle,
                } => {
                    self.workspace_lifecycle_revision = None;
                }
                _ => {}
            }
        }
        answer
    }

    fn attach_pane_cursors(&mut self, reply: &mut Reply, services: &Services<'_>) {
        if !matches!(reply, Reply::Text(_) | Reply::Panes(_)) {
            return;
        }
        let topology = services.terminal.topology().ok().map(|topology| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_vec(&topology).unwrap_or_default().hash(&mut hash);
            hash.finish()
        });
        let Ok(inventory) = services.terminal.pane_inventory() else {
            return;
        };
        match reply {
            Reply::Text(text) => {
                if let Some(pane) = inventory.panes.iter().find(|pane| pane.slot == text.slot) {
                    let (_, revision, generation, _) = self.pane_state(services, pane, topology);
                    text.generation = generation;
                    text.revision = revision;
                }
            }
            Reply::Panes(returned) => {
                for pane in &mut returned.panes {
                    let (_, revision, generation, _) = self.pane_state(services, pane, topology);
                    pane.generation = generation;
                    pane.revision = revision;
                }
            }
            _ => {}
        }
    }

    /// Returns the credit a peer released as it consumed frames. A payload that
    /// is not a count, or a channel that has since closed, is ignored: stale
    /// credit is ordinary on a channel the host already tore down.
    fn replenish(&mut self, frame: &Frame) -> Option<Topic> {
        let Ok(frames) = serde_json::from_slice::<u32>(&frame.payload) else {
            return None;
        };
        self.channels.replenish(frame.channel, frames).ok()?;
        let topic = self
            .session
            .topics()
            .into_iter()
            .find(|topic| self.subscriptions.channel(*topic) == Some(frame.channel))?;
        if self.outbox.depth(frame.channel) == 0 {
            return None;
        }
        matches!(self.channels.reserve(frame.channel), Ok(Permission::Send)).then_some(topic)
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
    fn gather(&mut self) -> Result<(), Fault> {
        self.queue.deposit(self.session.drain(), self.session.drain_sources())
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
            let payload = if topic == Topic::PaneChanges && message.superseded > 0 {
                serde_json::from_slice::<Snapshot>(&message.payload)
                    .map(|snapshot| snapshot.with_coalesced(message.superseded))
                    .and_then(|snapshot| serde_json::to_vec(&snapshot))
                    .unwrap_or(message.payload)
            } else {
                message.payload
            };
            let frame = Frame::new(channel, Kind::Event, payload);
            self.wire.send(&frame).map_err(fault)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use hl_extension::port::{
        ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
        PaneSummary, TabSummary, TerminalSurface, WorkspaceFiles,
    };
    use hl_extension::{
        Authority, Capability, ExtensionName, Failure, Frame, Grant, Hello, Kind, PROTOCOL, RelativePath, Reply,
        Request, Services, Transit, Wire, WorkspaceInfo, codec,
    };

    use super::{Compatibility, Conversation, Emission, Fault, Queue, Snapshot};

    /// What the adapters were actually asked for, so a refusal that still
    /// reached a service would be visible rather than silent.
    #[derive(Debug, Default)]
    struct Ledger {
        reached: Mutex<Vec<&'static str>>,
        semantic_revision: AtomicU64,
    }

    impl Ledger {
        fn note(&self, what: &'static str) {
            self.reached.lock().expect("ledger").push(what);
        }

        fn reached(&self) -> Vec<&'static str> {
            self.reached.lock().expect("ledger").clone()
        }

        fn semantic_revision(&self, revision: u64) {
            self.semantic_revision.store(revision, Ordering::Release);
        }
    }

    /// In-memory adapters: no container runtime and no window.
    struct Host {
        ledger: Arc<Ledger>,
    }
    impl hl_extension::port::VolumeStore for Host {}
    impl hl_extension::port::NetworkStore for Host {}

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

        fn executions(&self) -> Result<hl_extension::port::ExecutionList, HostError> {
            self.ledger.note("executions.list");
            Ok(hl_extension::port::ExecutionList {
                executions: vec![hl_extension::port::ExecutionSummary {
                    id: "e1".into(),
                    container_id: "c1".into(),
                    running: false,
                    exit_code: 7,
                    pid: 42,
                    command: vec!["worker".into()],
                    user: "root".into(),
                }],
                truncated: false,
            })
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
            let semantic = self.ledger.semantic_revision.load(Ordering::Acquire) != 0;
            Ok(vec![TabSummary {
                id: "t1".to_owned(),
                title: "shell".to_owned(),
                panes: vec![PaneSummary {
                    slot: "s1".to_owned(),
                    working_directory: None,
                    command: None,
                    occupant: if semantic {
                        hl_extension::port::Occupant::Surface
                    } else {
                        hl_extension::port::Occupant::Terminal
                    },
                    provider: None,
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

        fn read(&self, slot: &str, lines: usize) -> Result<hl_extension::port::PaneText, HostError> {
            self.ledger.note("terminal.read");
            Ok(hl_extension::port::PaneText {
                slot: slot.to_owned(),
                generation: 0,
                revision: 0,
                lines: vec![format!("at most {lines}")],
                cursor_column: 12,
                cursor_row: 3,
                truncated: false,
            })
        }

        fn semantics(&self, slot: &str) -> Result<hl_extension::PaneSemanticTree, HostError> {
            self.ledger.note("terminal.semantics");
            let revision = self.ledger.semantic_revision.load(Ordering::Acquire);
            if revision == 0 {
                return Err(HostError::Unsupported("pane semantics are unavailable".into()));
            }
            Ok(hl_extension::PaneSemanticTree {
                slot: slot.to_owned(),
                revision,
                root: hl_extension::SemanticNode {
                    id: 1,
                    role: "status".into(),
                    label: Some("Lifecycle notice".into()),
                    value: Some(format!("revision {revision}")),
                    disabled: false,
                    destructive: false,
                    actions: Vec::new(),
                    children: Vec::new(),
                },
                truncated: false,
            })
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
            self.ledger.note("terminal.ratio");
            Ok(())
        }

        fn surface(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
            self.ledger.note("terminal.surface");
            Ok("s3".to_owned())
        }
    }

    impl hl_extension::port::WorkspaceInventory for Host {
        fn workspaces(&self) -> Result<Vec<hl_extension::port::WorkspaceState>, HostError> {
            self.ledger.note("workspace.list");
            Ok(Vec::new())
        }
    }

    impl hl_extension::port::WorkspaceControl for Host {}

    struct LifecycleHost(Vec<hl_extension::WorkspaceLifecycleChange>);

    impl hl_extension::port::WorkspaceControl for LifecycleHost {
        fn lifecycle_revision(&self) -> u64 {
            self.0.last().map_or(0, |change| change.revision)
        }

        fn lifecycle_since(&self, revision: u64) -> Result<Vec<hl_extension::WorkspaceLifecycleChange>, HostError> {
            Ok(self
                .0
                .iter()
                .filter(|change| change.revision > revision)
                .cloned()
                .collect())
        }
    }

    struct SharedLifecycleHost;

    impl hl_extension::port::WorkspaceControl for SharedLifecycleHost {
        fn lifecycle_revision(&self) -> u64 {
            crate::workspace_lifecycle::revision()
        }

        fn lifecycle_since(&self, revision: u64) -> Result<Vec<hl_extension::WorkspaceLifecycleChange>, HostError> {
            Ok(crate::workspace_lifecycle::since(revision))
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
                size: 8,
            })
        }

        fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
            self.ledger.note("files.write");
            Ok(())
        }
    }

    impl hl_extension::port::ExtensionStore for Host {
        fn list(&self) -> Result<Vec<hl_extension::port::ExtensionSummary>, HostError> {
            self.ledger.note("extensions.list");
            Ok(vec![hl_extension::port::ExtensionSummary {
                name: "workspace-manager".into(),
                image_digest: "sha256:manager".into(),
                status: "duty".into(),
            }])
        }
    }

    fn services(host: &Host) -> Services<'_> {
        Services {
            workspace: WorkspaceInfo {
                name: "dev".to_owned(),
                architecture: "arm64".to_owned(),
                image: "alpine:3.20".to_owned(),
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

    /// The grant every test starts from: read containers/extensions, and draw.
    fn authority() -> Authority {
        Authority::new(
            ExtensionName::new("sample").expect("name"),
            Grant::new([
                Capability::ContainerRead,
                Capability::ExtensionRead,
                Capability::ExtensionInstall,
                Capability::Interface,
            ]),
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

    #[test]
    fn conversation_drains_the_composed_native_extension_source() {
        let (_theirs, ours) = UnixStream::pair().expect("socket pair");
        let mut conversation = Conversation::new(ours, authority(), "dev", Queue::new()).expect("conversation");
        let events = super::super::management_events::ExtensionEvents::default();
        events.inventory(vec![hl_extension::port::ExtensionSummary {
            name: "workspace-manager".into(),
            image_digest: "sha256:observed".into(),
            status: "duty".into(),
        }]);
        conversation.with_extension_events(events);

        let batch = conversation.drain_extension_events().expect("native extension change");
        assert_eq!(batch.inventory.expect("inventory")[0].name, "workspace-manager");
        assert!(
            conversation.drain_extension_events().is_none(),
            "the adapter drains rather than polls history"
        );
    }

    #[test]
    fn queued_ui_interaction_crosses_one_framed_writer_with_slot_identity() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let mut wire = Wire::new(theirs);
        let mut conversation = Conversation::new(ours, authority(), "dev", Queue::new()).expect("conversation");
        let voice = super::super::host::Voice::default();
        voice.hold();
        conversation.with_voice(voice.clone());
        super::super::host::speak_at(
            &voice,
            &hl_extension::SurfaceEvent {
                slot: "surface-9".into(),
                event: hl_gui::Event::Focus {
                    node: hl_gui::NodeId::new(4),
                    id: hl_gui::EventId::new("editor"),
                    focused: true,
                },
            },
        );

        conversation.flush_interactions().expect("single writer flush");
        let frame = wire.receive().expect("framed event");
        assert_eq!(frame.kind, Kind::Event);
        let event: serde_json::Value = serde_json::from_slice(&frame.payload).expect("event json");
        assert_eq!(event["slot"], "surface-9");
        assert_eq!(event["interaction"], "focus");
    }

    #[test]
    fn native_acquisition_invalidations_cross_the_credit_controlled_event_channel() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        theirs
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("peer deadline");
        let mut wire = Wire::new(theirs);
        let mut conversation = Conversation::new(ours, authority(), "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::ExtensionAcquisitions);
        let events = super::super::management_events::ExtensionEvents::default();
        let job = super::super::acquisition::AcquisitionJob::parse("7").expect("job");
        events.acquisition(
            job,
            super::super::acquisition::AcquisitionSnapshot {
                reference: "registry/tool:1".into(),
                revision: 3,
                state: super::super::acquisition::AcquisitionState::ReadingManifest,
            },
        );
        conversation.with_extension_events(events);
        let ledger = Arc::new(Ledger::default());
        let host = Host { ledger };

        conversation.observe(&services(&host)).expect("native event observed");
        let frame = wire.receive().expect("acquisition event");
        let snapshot: Snapshot = serde_json::from_slice(&frame.payload).expect("typed snapshot");
        assert!(matches!(
            snapshot,
            Snapshot::ExtensionAcquisitions(change)
                if change.job == "7" && change.revision == 3 && change.state == "reading-manifest"
        ));
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
    fn a_production_subscription_receives_changed_full_snapshots_without_duplicates() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        theirs
            .set_read_timeout(Some(Duration::from_millis(650)))
            .expect("peer deadline");
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(
            &mut wire,
            &Request::EventSubscribe {
                topic: hl_extension::Topic::Containers,
            },
        );
        assert_eq!(codec::read_reply(&answer).expect("subscription reply"), Reply::Done);

        let event = wire.receive().expect("initial production snapshot");
        assert_eq!(event.kind, Kind::Event);
        let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("typed snapshot");
        assert!(matches!(snapshot, Snapshot::Containers(containers) if containers.len() == 1));

        assert_eq!(
            wire.receive(),
            Err(Transit::Pending),
            "an unchanged listing is observed but not published again"
        );
        assert!(
            ledger
                .reached()
                .iter()
                .filter(|call| **call == "containers.list")
                .count()
                >= 2,
            "the absence of a duplicate is from equality, not a stopped producer"
        );
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn execution_observation_is_subscriber_driven_and_delivers_bounded_identity_state() {
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let mut conversation = Conversation::new(ours, authority(), "dev", Queue::new()).expect("conversation");
        let host = Host {
            ledger: Arc::clone(&ledger),
        };
        conversation.observe(&services(&host)).expect("idle observation");
        assert!(!ledger.reached().contains(&"executions.list"));
        conversation.session.follow(hl_extension::Topic::Executions);
        conversation.observe(&services(&host)).expect("subscribed observation");
        let event = Wire::new(theirs).receive().expect("execution snapshot");
        let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("typed snapshot");
        assert!(
            matches!(snapshot, Snapshot::Executions(list) if list.executions[0].id == "e1" && !list.executions[0].running)
        );
    }

    #[test]
    fn an_extension_inventory_subscription_receives_one_changed_bounded_listing() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        theirs
            .set_read_timeout(Some(Duration::from_millis(650)))
            .expect("peer deadline");
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(
            &mut wire,
            &Request::EventSubscribe {
                topic: hl_extension::Topic::Extensions,
            },
        );
        assert_eq!(codec::read_reply(&answer).expect("subscription reply"), Reply::Done);
        let event = wire.receive().expect("initial extension snapshot");
        let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("typed snapshot");
        assert!(
            matches!(snapshot, Snapshot::Extensions(extensions) if extensions.len() == 1 && extensions[0].name == "workspace-manager")
        );
        assert_eq!(
            wire.receive(),
            Err(Transit::Pending),
            "unchanged inventory is coalesced"
        );
        assert!(
            ledger
                .reached()
                .iter()
                .filter(|call| **call == "extensions.list")
                .count()
                > 1
        );
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn production_publication_waits_for_returned_credit_and_releases_the_latest_snapshot() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        theirs
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("peer deadline");
        let mut conversation = Conversation::new(ours, authority(), "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::Containers);
        let snapshot = |created| {
            Snapshot::Containers(vec![ContainerSummary {
                id: "c1".into(),
                name: "api".into(),
                image: "image".into(),
                state: "running".into(),
                created,
            }])
        };

        for created in 0..hl_extension::Channels::CREDIT {
            assert_eq!(
                conversation.publish(&snapshot(i64::from(created))),
                Ok(Emission::Queued)
            );
        }
        assert_eq!(conversation.publish(&snapshot(99)), Ok(Emission::Superseded));
        let channel = conversation
            .subscriptions
            .channel(hl_extension::Topic::Containers)
            .expect("subscription route");
        assert_eq!(conversation.outbox.depth(channel), 1, "only the latest state waits");

        let mut peer = Wire::new(theirs);
        for _ in 0..hl_extension::Channels::CREDIT {
            assert_eq!(peer.receive().expect("credited event").kind, Kind::Event);
        }
        assert_eq!(
            peer.receive(),
            Err(Transit::Pending),
            "the uncredited event was not sent"
        );

        let credit = Frame::new(channel, Kind::Credit, serde_json::to_vec(&1_u32).expect("credit"));
        let host = Host {
            ledger: Arc::new(Ledger::default()),
        };
        conversation
            .exchange(&credit, &services(&host))
            .expect("credit returned");
        let released = peer.receive().expect("latest event released");
        let latest: Snapshot = serde_json::from_slice(&released.payload).expect("snapshot");
        assert!(matches!(latest, Snapshot::Containers(containers) if containers[0].created == 99));
    }

    #[test]
    fn pane_observation_does_no_terminal_or_semantic_work_without_a_subscriber() {
        let ledger = Arc::new(Ledger::default());
        let host = Host {
            ledger: Arc::clone(&ledger),
        };
        let (ours, _theirs) = UnixStream::pair().expect("socket pair");
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::PaneObserve]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        conversation.observe(&services(&host)).expect("idle observation");
        assert!(ledger.reached().is_empty(), "no subscription means zero adapter calls");
    }

    #[test]
    fn workspace_lifecycle_observation_uses_read_authority_and_preserves_revisions() {
        let host = Host {
            ledger: Arc::new(Ledger::default()),
        };
        let lifecycle = LifecycleHost(vec![
            hl_extension::WorkspaceLifecycleChange {
                workspace: "other".into(),
                action: hl_extension::WorkspaceLifecycleAction::Create,
                revision: 4,
                coalesced: 0,
            },
            hl_extension::WorkspaceLifecycleChange {
                workspace: "target".into(),
                action: hl_extension::WorkspaceLifecycleAction::Start,
                revision: 5,
                coalesced: 0,
            },
        ]);
        let (ours, peer) = UnixStream::pair().expect("socket pair");
        peer.set_read_timeout(Some(Duration::from_secs(1))).expect("timeout");
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::WorkspaceRead]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::WorkspaceLifecycle);
        conversation.workspace_lifecycle_revision = Some(3);
        let mut ports = services(&host);
        ports.workspace_control = &lifecycle;
        conversation.observe(&ports).expect("observe lifecycle");
        let mut wire = Wire::new(peer);
        let first: Snapshot = serde_json::from_slice(&wire.receive().expect("first").payload).expect("snapshot");
        let second: Snapshot = serde_json::from_slice(&wire.receive().expect("second").payload).expect("snapshot");
        assert!(
            matches!(first, Snapshot::WorkspaceLifecycle(change) if change.workspace == "other" && change.revision == 4)
        );
        assert!(
            matches!(second, Snapshot::WorkspaceLifecycle(change) if change.workspace == "target" && change.revision == 5)
        );
    }

    #[test]
    fn a_native_store_mutation_reaches_the_same_subscriber_as_an_mcp_mutation() {
        let host = Host {
            ledger: Arc::new(Ledger::default()),
        };
        let lifecycle = SharedLifecycleHost;
        let (ours, peer) = UnixStream::pair().expect("socket pair");
        peer.set_read_timeout(Some(Duration::from_secs(1))).expect("timeout");
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::WorkspaceRead]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::WorkspaceLifecycle);
        conversation.workspace_lifecycle_revision =
            Some(hl_extension::port::WorkspaceControl::lifecycle_revision(&lifecycle));

        let name = format!("native-observed-{}", std::process::id());
        let path = std::env::temp_dir().join(format!("husklet-{name}.conf"));
        let _ = std::fs::remove_file(&path);
        crate::config::WorkspaceStore::load(&path)
            .and_then(|mut store| {
                store.upsert(crate::config::WorkspaceConfig::new(
                    &name,
                    "alpine:3.20",
                    hl_ws::Arch::Amd64,
                ))
            })
            .expect("native persistence");

        let mut ports = services(&host);
        ports.workspace_control = &lifecycle;
        conversation.observe(&ports).expect("observe native mutation");
        let mut wire = Wire::new(peer);
        let observed = (0..256).any(|_| {
            let event: Snapshot = serde_json::from_slice(&wire.receive().expect("event").payload).expect("snapshot");
            matches!(event, Snapshot::WorkspaceLifecycle(change)
                if change.workspace == name && change.action == hl_extension::WorkspaceLifecycleAction::Create)
        });
        assert!(
            observed,
            "native mutation was delivered through the shared lifecycle ledger"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_from_now_lifecycle_subscription_does_not_replay_the_hosts_prior_revision() {
        let host = Host {
            ledger: Arc::new(Ledger::default()),
        };
        let lifecycle = LifecycleHost(vec![hl_extension::WorkspaceLifecycleChange {
            workspace: "before-subscribe".into(),
            action: hl_extension::WorkspaceLifecycleAction::Create,
            revision: 71,
            coalesced: 0,
        }]);
        let (ours, peer) = UnixStream::pair().expect("socket pair");
        peer.set_nonblocking(true).expect("nonblocking peer");
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::WorkspaceRead]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        let mut ports = services(&host);
        ports.workspace_control = &lifecycle;
        let subscribe = codec::request(&Request::EventSubscribe {
            topic: hl_extension::Topic::WorkspaceLifecycle,
        })
        .expect("subscribe request");
        conversation.exchange(&subscribe, &ports).expect("subscribe");
        let mut wire = Wire::new(peer);
        assert_eq!(
            codec::read_reply(&wire.receive().expect("reply")).expect("done"),
            Reply::Done
        );
        conversation.observe(&ports).expect("observe from now");
        assert_eq!(
            wire.receive(),
            Err(Transit::Pending),
            "history is not replayed to a new subscriber"
        );
    }

    #[test]
    fn pane_observation_is_credit_gated_and_reports_transport_coalescing() {
        let ledger = Arc::new(Ledger::default());
        let host = Host {
            ledger: Arc::clone(&ledger),
        };
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::PaneObserve]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::PaneChanges);
        for generation in 0..=(hl_extension::Channels::CREDIT + 1) {
            let change = hl_extension::PaneChange {
                slot: "s1".into(),
                kind: hl_extension::PaneChangeKind::Terminal,
                revision: 0,
                generation: u64::from(generation),
                coalesced: 0,
            };
            conversation.publish(&Snapshot::PaneChanges(change)).expect("publish");
        }
        let channel = conversation
            .subscriptions
            .channel(hl_extension::Topic::PaneChanges)
            .expect("route");
        ledger.reached.lock().expect("ledger").clear();
        conversation
            .observe_panes(&services(&host))
            .expect("stalled observation");
        assert!(ledger.reached().is_empty(), "zero credit means zero GTK adapter work");

        let mut peer = Wire::new(theirs);
        for _ in 0..hl_extension::Channels::CREDIT {
            peer.receive().expect("credited event");
        }
        let credit = Frame::new(channel, Kind::Credit, serde_json::to_vec(&1_u32).expect("credit"));
        conversation.exchange(&credit, &services(&host)).expect("return credit");
        let released = peer.receive().expect("coalesced event");
        let snapshot: Snapshot = serde_json::from_slice(&released.payload).expect("snapshot");
        assert!(matches!(snapshot, Snapshot::PaneChanges(change) if change.coalesced == 1));
    }

    #[test]
    fn semantic_revisions_emit_ordered_bounded_pane_invalidations_and_coalesce_between_scans() {
        let ledger = Arc::new(Ledger::default());
        ledger.semantic_revision(1);
        let host = Host {
            ledger: Arc::clone(&ledger),
        };
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        theirs
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("peer deadline");
        let mut peer = Wire::new(theirs);
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::PaneObserve]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::PaneChanges);
        conversation
            .route(hl_extension::Topic::PaneChanges)
            .expect("pane event route");
        let channel = conversation
            .subscriptions
            .channel(hl_extension::Topic::PaneChanges)
            .expect("pane event channel");
        let credit = Frame::new(channel, Kind::Credit, serde_json::to_vec(&2_u32).expect("credit"));
        conversation
            .exchange(&credit, &services(&host))
            .expect("event credit accepted");
        assert!(conversation.channels.credit(channel).is_some_and(|credit| credit >= 2));

        conversation.pane_next = std::time::Instant::now() - Duration::from_secs(1);
        conversation
            .observe_panes(&services(&host))
            .expect("initial semantic observation");
        assert!(ledger.reached().contains(&"terminal.semantics"));
        let initial = peer.receive().expect("initial pane invalidation");
        let initial_snapshot: Snapshot = serde_json::from_slice(&initial.payload).expect("typed invalidation");
        assert!(matches!(
            initial_snapshot,
            Snapshot::PaneChanges(ref change)
                if change.slot == "s1"
                    && change.kind == hl_extension::PaneChangeKind::Native
                    && change.revision == 1
                    && change.generation == 1
        ));
        assert!(
            initial.payload.len() <= Frame::PAYLOAD_LIMIT,
            "metadata remains protocol bounded"
        );

        // Two UI mutations before the next host scan collapse into the latest
        // revision: contents remain behind PaneSemanticRead and no stale
        // intermediate revision can overtake it on the event channel.
        ledger.semantic_revision(2);
        ledger.semantic_revision(3);
        conversation.pane_next = std::time::Instant::now() - Duration::from_secs(1);
        conversation
            .observe_panes(&services(&host))
            .expect("changed semantic observation");
        let latest = peer.receive().expect("latest pane invalidation");
        let latest_snapshot: Snapshot = serde_json::from_slice(&latest.payload).expect("typed invalidation");
        assert!(matches!(
            latest_snapshot,
            Snapshot::PaneChanges(ref change)
                if change.revision == 3 && change.generation == 2 && change.coalesced == 0
        ));
        assert!(
            latest.payload.len() <= Frame::PAYLOAD_LIMIT,
            "metadata remains protocol bounded"
        );
        assert_eq!(
            peer.receive(),
            Err(Transit::Pending),
            "the skipped revision was coalesced at observation"
        );
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
        assert_eq!(collected.frames[0].slot, "tab-Sample");
        assert_eq!(collected.frames[0].frame.sequence, 1);
        assert!(queue.is_empty(), "collecting empties the queue");
        drop(wire);
        let _ = served.join().expect("joined");
    }

    #[test]
    fn one_extensions_interface_backlog_is_hard_bounded_and_does_not_consume_anothers() {
        let noisy = Queue::new();
        let healthy = Queue::new();
        let frames = (0..Queue::LIMIT)
            .map(|sequence| hl_extension::SurfaceFrame {
                slot: "noisy".into(),
                frame: hl_gui::Frame::new(u64::try_from(sequence).expect("bounded sequence")),
            })
            .collect();
        noisy.deposit(frames, Vec::new()).expect("the exact bound is admitted");
        let overflow = noisy.deposit(
            vec![hl_extension::SurfaceFrame {
                slot: "noisy".into(),
                frame: hl_gui::Frame::new(999),
            }],
            Vec::new(),
        );
        assert!(matches!(overflow, Err(Fault::Malformed(ref detail)) if detail.contains("window catch up")));
        assert_eq!(
            noisy.collect().frames.len(),
            Queue::LIMIT,
            "overflow is rejected atomically"
        );

        healthy
            .deposit(
                vec![hl_extension::SurfaceFrame {
                    slot: "healthy".into(),
                    frame: hl_gui::Frame::new(1),
                }],
                Vec::new(),
            )
            .expect("another extension owns an independent budget");
        assert_eq!(healthy.collect().frames.len(), 1);
    }

    #[test]
    fn one_oversized_frame_cannot_hide_unbounded_gtk_work_inside_one_queue_entry() {
        let noisy = Queue::new();
        let healthy = Queue::new();
        let oversized = hl_gui::Frame {
            sequence: 1,
            patches: (0..=Queue::LIMIT)
                .map(|_| hl_gui::Patch::Remove {
                    id: hl_gui::NodeId::new(1),
                })
                .collect(),
        };

        let overflow = noisy.deposit(
            vec![hl_extension::SurfaceFrame {
                slot: "noisy".into(),
                frame: oversized,
            }],
            Vec::new(),
        );
        assert!(matches!(overflow, Err(Fault::Malformed(ref detail)) if detail.contains("window catch up")));
        assert!(noisy.is_empty(), "an oversized frame is rejected atomically");

        healthy
            .deposit(
                vec![hl_extension::SurfaceFrame {
                    slot: "healthy".into(),
                    frame: hl_gui::Frame::new(1),
                }],
                Vec::new(),
            )
            .expect("a sibling extension retains its independent render budget");
        assert_eq!(healthy.collect().frames.len(), 1);
    }

    #[test]
    fn one_row_window_cannot_hide_unbounded_gtk_work_inside_one_mutation() {
        let queue = Queue::new();
        let rows = (0..=Queue::LIMIT)
            .map(|index| hl_gui::Row::new(index as u64, [hl_gui::Cell::text(index.to_string())]))
            .collect();
        let mutation = hl_extension::SurfaceMutation {
            slot: "table-pane".into(),
            mutation: hl_gui::SourceMutation::Window(hl_gui::RowWindow {
                source: hl_gui::SourceId::new(1),
                version: hl_gui::Version::new(1),
                request: hl_gui::RequestId::new(1),
                range: hl_gui::RowRange::new(0, hl_gui::RowRange::BLOCK),
                rows,
            }),
        };

        let overflow = queue.deposit(Vec::new(), vec![mutation]);
        assert!(matches!(overflow, Err(Fault::Malformed(ref detail)) if detail.contains("window catch up")));
        assert!(queue.is_empty(), "the oversized source answer is rejected atomically");
    }

    #[test]
    fn an_oversized_cell_faults_before_the_row_window_is_queued() {
        let queue = Queue::new();
        let mutation = hl_extension::SurfaceMutation {
            slot: "table-pane".into(),
            mutation: hl_gui::SourceMutation::Window(hl_gui::RowWindow {
                source: hl_gui::SourceId::new(1),
                version: hl_gui::Version::new(1),
                request: hl_gui::RequestId::new(1),
                range: hl_gui::RowRange::new(0, 1),
                rows: vec![hl_gui::Row::new(
                    0,
                    [hl_gui::Cell::text("x".repeat(hl_gui::Cell::MAX_TEXT_BYTES + 1))],
                )],
            }),
        };

        let overflow = queue.deposit(Vec::new(), vec![mutation]);
        assert!(matches!(overflow, Err(Fault::Malformed(ref detail)) if detail.contains("text payload")));
        assert!(queue.is_empty(), "invalid text never becomes pending GTK work");
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
