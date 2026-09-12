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
    Authority, ChannelId, Channels, Compatibility, Emission, Failure, Frame, Hello, Kind, Limits, Outbox, PROTOCOL,
    PaneChange, PaneChangeKind, Permission, Reply, Services, Session, Snapshot, Streams, Subscriptions, SurfaceFrame,
    SurfaceMutation, Topic, Transit, Welcome, Wire, codec,
};

/// Interface work an extension has produced and the GUI has not collected yet.
#[derive(Debug, Default)]
pub struct Interface {
    /// Descriptions of what to draw, in the order the extension sent them.
    pub frames: Vec<SurfaceFrame>,
    /// Changes to the windowed sources the extension's tables draw from.
    pub mutations: Vec<SurfaceMutation>,
    pub notifications: Vec<hl_extension::Notification>,
}

impl Interface {
    /// Whether there is anything for the GUI to apply.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty() && self.mutations.is_empty() && self.notifications.is_empty()
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
    ///
    /// A single rich, atomic React commit can legitimately describe hundreds
    /// of native nodes. The delivery queue still bounds frames separately and
    /// GTK drains only eight per tick, while this weighted ceiling prevents a
    /// large initial tree from being mistaken for a runaway producer.
    pub const LIMIT: usize = 4_096;
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
            notifications: std::mem::take(&mut held.notifications),
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
        if mutations.iter().any(|mutation| {
            matches!(&mutation.mutation, hl_gui::SourceMutation::Open { columns, .. }
                if hl_gui::validate_columns(columns).is_err())
        }) {
            return Err(Fault::Malformed("an invalid table schema was refused".into()));
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

impl hl_extension::NotificationSink for Queue {
    fn publish(&self, notification: &hl_extension::Notification) -> Result<(), hl_extension::HostError> {
        let mut held = self.hold();
        if let Some(current) = held
            .notifications
            .iter_mut()
            .find(|current| current.id == notification.id)
        {
            current.clone_from(notification);
            return Ok(());
        }
        if held.notifications.len() >= 32 {
            return Err(hl_extension::HostError::Failed("notification queue is full".into()));
        }
        held.notifications.push(notification.clone());
        Ok(())
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
    image_pull_cursor: u64,
    events: Option<super::host::Events>,
    voice: Option<super::host::Voice>,
}

impl Conversation {
    /// How often a serving conversation yields to observation and the maximum
    /// time one socket write may monopolize its worker.
    const IO_TURN: Duration = Duration::from_millis(250);
    /// How long a connected peer has to complete the handshake.
    ///
    /// A process that connects and says nothing would otherwise hold the one
    /// conversation an extension gets for as long as it likes.
    pub const SETTLE: Duration = Duration::from_secs(5);

    /// Wraps an accepted connection for one extension.
    ///
    /// # Errors
    /// Returns the failure to duplicate the socket descriptor.
    #[cfg(test)]
    pub fn new(
        stream: UnixStream,
        authority: Authority,
        workspace: impl Into<String>,
        queue: Queue,
    ) -> io::Result<Self> {
        let roots = authority
            .roots()
            .iter()
            .cloned()
            .map(|subtree| hl_extension::FilesystemSelector::Subtree { subtree })
            .collect::<Vec<_>>();
        Self::new_scoped_owned(
            stream,
            authority,
            "test",
            workspace,
            queue,
            hl_extension::ContainerGrant {
                selectors: vec![hl_extension::ContainerSelector::All { all: true }],
                create: true,
            },
            hl_extension::ImageGrant {
                read: vec![hl_extension::ImageSelector::All { all: true }],
                r#use: vec![hl_extension::ImageSelector::All { all: true }],
                pull: vec![hl_extension::ImageSelector::All { all: true }],
                remove: vec![hl_extension::ImageSelector::All { all: true }],
                prune_all_unused: true,
            },
            hl_extension::NetworkGrant {
                selectors: vec![hl_extension::NetworkSelector::All { all: true }],
                create: true,
            },
            hl_extension::VolumeGrant {
                selectors: vec![hl_extension::VolumeSelector::All { all: true }],
                create: true,
            },
            hl_extension::FilesystemGrant {
                read: roots.clone(),
                write: roots.clone(),
                create: roots.clone(),
                delete: roots.clone(),
                rename: roots,
            },
            hl_extension::WorkspaceEnvironmentGrant::default(),
        )
    }

    /// Wraps a connection with its separately persisted container-resource consent.
    pub fn new_scoped_owned(
        stream: UnixStream,
        authority: Authority,
        extension_identity: impl Into<String>,
        workspace: impl Into<String>,
        queue: Queue,
        containers: hl_extension::ContainerGrant,
        images: hl_extension::ImageGrant,
        networks: hl_extension::NetworkGrant,
        volumes: hl_extension::VolumeGrant,
        filesystem: hl_extension::FilesystemGrant,
        workspace_environment: hl_extension::WorkspaceEnvironmentGrant,
    ) -> io::Result<Self> {
        let control = stream.try_clone()?;
        Ok(Self {
            wire: Wire::new(stream),
            control,
            // The workspace overview exists before the sidecar connects. Its
            // empty slot is the extension's primary surface; extra tab/split
            // surfaces are acquired explicitly through the terminal port.
            session: Session::new(authority)
                .with_extension_identity(extension_identity)
                .with_containers(containers)
                .with_images(images)
                .with_networks(networks)
                .with_volumes(volumes)
                .with_filesystem(filesystem)
                .with_workspace_environment(workspace_environment)
                .with_surface(""),
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
            image_pull_cursor: 0,
            events: None,
            voice: None,
        })
    }

    #[cfg(test)]
    pub fn new_scoped(
        stream: UnixStream,
        authority: Authority,
        workspace: impl Into<String>,
        queue: Queue,
        containers: hl_extension::ContainerGrant,
        networks: hl_extension::NetworkGrant,
        volumes: hl_extension::VolumeGrant,
        filesystem: hl_extension::FilesystemGrant,
        workspace_environment: hl_extension::WorkspaceEnvironmentGrant,
    ) -> io::Result<Self> {
        Self::new_scoped_owned(
            stream,
            authority,
            "test",
            workspace,
            queue,
            containers,
            hl_extension::ImageGrant::default(),
            networks,
            volumes,
            filesystem,
            workspace_environment,
        )
    }

    pub(crate) fn with_events(&mut self, events: super::host::Events) {
        self.events = Some(events);
    }

    pub(crate) fn notifications(&self) -> Queue {
        self.queue.clone()
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
        self.arm_io_deadlines()?;
        let mut partial_since = None;
        loop {
            match self.wire.receive_step() {
                Ok(frame) => {
                    partial_since = None;
                    // A control close ends this connection generation. Bytes may
                    // already be buffered behind it in the same socket read, but
                    // they belong to a peer that has surrendered its authority
                    // and must never reach either services or the GUI queue.
                    if frame.kind == Kind::Close && frame.channel == ChannelId::CONTROL {
                        return Ok(());
                    }
                    self.exchange(&frame, services)?;
                }
                Err(Transit::Pending) => {
                    if self.wire.buffered() == 0 {
                        partial_since = None;
                    } else if partial_since.get_or_insert_with(Instant::now).elapsed() >= self.settle {
                        return Err(Fault::Malformed("an unfinished frame exceeded its deadline".into()));
                    }
                    self.observe(services)?;
                }
                Err(Transit::Closed) => return Ok(()),
                Err(other) => return Err(fault(other)),
            }
        }
    }

    fn arm_io_deadlines(&self) -> io::Result<()> {
        self.control.set_read_timeout(Some(Self::IO_TURN))?;
        self.control.set_write_timeout(Some(Self::IO_TURN))
    }

    /// Publishes changed full listings for topics backed by real production ports.
    ///
    /// Failed reads produce no event: publishing an empty list would falsely
    /// report that resources disappeared. `publish` retains the existing
    /// capability check, channel credit, and latest-snapshot coalescing.
    fn observe(&mut self, services: &Services<'_>) -> Result<(), Fault> {
        self.flush_interactions()?;
        let mut snapshots = Vec::new();
        if self.may_observe(Topic::Containers) {
            if let Ok(containers) = services.containers.list() {
                snapshots.push(Snapshot::Containers(self.session.visible_containers(containers)));
            }
        }
        if self.may_observe(Topic::ContainerInventory) {
            if let Ok(mut containers) = services.containers.list() {
                containers = self.session.visible_containers(containers);
                const LIMIT: usize = 256;
                let complete = containers.len() <= LIMIT;
                containers.truncate(LIMIT);
                snapshots.push(Snapshot::ContainerInventory(hl_extension::ContainerInventory {
                    containers,
                    complete,
                }));
            }
        }
        if self.may_observe(Topic::Executions) {
            if let (Ok(executions), Ok(containers)) = (services.containers.executions(), services.containers.list()) {
                snapshots.push(Snapshot::Executions(
                    self.session.visible_executions(executions, &containers),
                ));
            }
        }
        if self.may_observe(Topic::Images) {
            if let Ok(images) = services.images.list() {
                snapshots.push(Snapshot::Images(self.session.visible_images(images)));
            }
        }
        if self.may_observe(Topic::ImagePulls) {
            let available = self.available_credit(Topic::ImagePulls);
            snapshots.extend(
                services
                    .images
                    .pull_changes(self.session.extension_identity(), self.image_pull_cursor)
                    .into_iter()
                    .take(available)
                    .map(Snapshot::ImagePulls),
            );
        }
        if self.may_observe(Topic::Volumes) {
            if let Ok(volumes) = services.volumes.list() {
                snapshots.push(Snapshot::Volumes(hl_extension::port::VolumeInventory::bounded(volumes)));
            }
        }
        if self.may_observe(Topic::Networks) {
            if let Ok(networks) = services.networks.list() {
                snapshots.push(Snapshot::Networks(self.session.visible_networks(networks)));
            }
        }
        if self.may_observe(Topic::Terminal) {
            if let Ok(tabs) = services.terminal.tabs() {
                snapshots.push(Snapshot::Terminal(tabs));
            }
        }
        if self.may_observe(Topic::Extensions) {
            if let Ok(extensions) = services.extensions.list() {
                snapshots.push(Snapshot::Extensions(extensions));
            }
        }
        if self.may_observe(Topic::ExtensionAcquisitions) {
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
        if self.may_observe(Topic::WorkspaceEvents) {
            if let Some(batch) = self.events.as_ref().and_then(super::host::Events::drain) {
                snapshots.push(Snapshot::WorkspaceEvents(batch));
            }
        }
        if self.may_observe(Topic::WorkspaceLifecycle) {
            if let Some(revision) = self.workspace_lifecycle_revision {
                if let Ok(changes) = services.workspace_control.lifecycle_since(revision) {
                    for change in changes
                        .into_iter()
                        .take(self.available_credit(Topic::WorkspaceLifecycle))
                    {
                        snapshots.push(Snapshot::WorkspaceLifecycle(change));
                    }
                }
            }
        }
        if self.may_observe(Topic::Filesystem) {
            if let Ok(inventory) = services.files.inventory(self.session.filesystem_read_selectors()) {
                snapshots.push(Snapshot::Filesystem(inventory));
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
            let emission = self.publish(&snapshot)?;
            if emission == Emission::Queued {
                match &snapshot {
                    Snapshot::ImagePulls(change) => {
                        self.image_pull_cursor = self.image_pull_cursor.max(change.sequence);
                    }
                    Snapshot::WorkspaceLifecycle(change) => {
                        self.workspace_lifecycle_revision = Some(change.revision);
                    }
                    _ => {}
                }
            }
            if topic != Topic::WorkspaceEvents && topic != Topic::WorkspaceLifecycle {
                self.observed.insert(topic, snapshot);
            }
        }
        self.observe_panes(services)?;
        Ok(())
    }

    /// Whether collecting a topic can produce an event the peer is currently
    /// allowed to receive. The first collection has no route yet; its first
    /// successful publication allocates one. After that, exhausted channel
    /// credit stops work at the service boundary rather than repeatedly
    /// rebuilding snapshots which cannot leave the host. Returned credit makes
    /// the next observation read fresh state, so a stalled peer resumes from
    /// the latest full snapshot.
    fn may_observe(&self, topic: Topic) -> bool {
        if !self.session.may_emit(topic) {
            return false;
        }
        self.subscriptions
            .channel(topic)
            .is_none_or(|channel| self.channels.credit(channel).is_some_and(|credit| credit > 0))
    }

    fn available_credit(&self, topic: Topic) -> usize {
        self.subscriptions
            .channel(topic)
            .and_then(|channel| self.channels.credit(channel))
            .unwrap_or(Channels::CREDIT)
            .try_into()
            .unwrap_or(usize::MAX)
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
            filesystem: self.session.filesystem_grant().clone(),
            limits: Limits::default(),
        };
        let frame = codec::welcome(&welcome).map_err(|coding| Fault::Malformed(coding.to_string()))?;
        self.wire.send(&frame).map_err(fault)
    }

    /// Reads the reply under a deadline, so an unfinished handshake ends the
    /// connection instead of holding it.
    fn hello(&mut self) -> Result<Hello, Fault> {
        let started = Instant::now();
        let received = loop {
            let remaining = self.settle.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break Err(Transit::Pending);
            }
            self.control.set_read_timeout(Some(remaining.min(Self::IO_TURN)))?;
            match self.wire.receive_step() {
                Err(Transit::Pending) if started.elapsed() < self.settle => {}
                result => break result,
            }
        };
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
        if matches!(
            frame.kind,
            Kind::Response | Kind::Event | Kind::Open | Kind::Reset | Kind::Pong
        ) {
            return Err(Fault::Malformed(format!(
                "extension sent unexpected {:?} frame on channel {:?}",
                frame.kind, frame.channel
            )));
        }
        if frame.kind == Kind::Ping {
            self.wire
                .send(&Frame::new(frame.channel, Kind::Pong, frame.payload.clone()))
                .map_err(fault)?;
            return Ok(());
        }
        if frame.kind == Kind::Close {
            if let Some(topic) = self
                .session
                .topics()
                .into_iter()
                .find(|topic| self.subscriptions.channel(*topic) == Some(frame.channel))
            {
                self.session.unfollow(topic);
                if topic == Topic::WorkspaceLifecycle {
                    self.workspace_lifecycle_revision = None;
                }
                self.subscriptions.close(topic, &mut self.channels, &mut self.outbox);
            }
            return Ok(());
        }
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
        let mut request = codec::read_request(frame).map_err(|coding| Failure::Unsupported {
            call: coding.to_string(),
        })?;
        self.session.authority().permit(request.capability())?;
        if let hl_extension::Request::TerminalSwitchOccupant { slot, generation, .. } = &mut request {
            let topology = services.terminal.topology().ok().map(|topology| {
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                serde_json::to_vec(&topology).unwrap_or_default().hash(&mut hash);
                hash.finish()
            });
            let inventory = services.terminal.pane_inventory()?;
            let pane = inventory
                .panes
                .iter()
                .find(|pane| pane.slot == *slot)
                .ok_or_else(|| Failure::Absent {
                    detail: format!("pane {slot} is absent"),
                })?;
            let (_, _, observed, _) = self.pane_state(services, pane, topology);
            if *generation != observed {
                return Err(Failure::Conflict {
                    detail: format!("pane {slot} changed since generation {generation}"),
                });
            }
            *generation = pane.generation;
        } else if let hl_extension::Request::TerminalWritePane {
            slot,
            generation,
            revision,
            ..
        }
        | hl_extension::Request::TerminalRetitlePaneObserved {
            slot,
            generation,
            revision,
            ..
        }
        | hl_extension::Request::TerminalFocusPaneObserved {
            slot,
            generation,
            revision,
        }
        | hl_extension::Request::TerminalResizeGridObserved {
            slot,
            generation,
            revision,
            ..
        }
        | hl_extension::Request::TerminalRatioObserved {
            slot,
            generation,
            revision,
            ..
        }
        | hl_extension::Request::TerminalSpawnObserved {
            slot,
            generation,
            revision,
            ..
        }
        | hl_extension::Request::TerminalClosePaneObserved {
            slot,
            generation,
            revision,
        }
        | hl_extension::Request::TerminalSplitObserved {
            slot,
            generation,
            revision,
            ..
        }
        | hl_extension::Request::TerminalSwitchOccupantObserved {
            slot,
            generation,
            revision,
            ..
        } = &mut request
        {
            let topology = services.terminal.topology().ok().map(|topology| {
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                serde_json::to_vec(&topology).unwrap_or_default().hash(&mut hash);
                hash.finish()
            });
            let inventory = services.terminal.pane_inventory()?;
            let pane = inventory
                .panes
                .iter()
                .find(|pane| pane.slot == *slot)
                .ok_or_else(|| Failure::Absent {
                    detail: format!("pane {slot} is absent"),
                })?;
            let (_, observed_revision, observed_generation, _) = self.pane_state(services, pane, topology);
            if *generation != observed_generation || *revision != observed_revision {
                return Err(Failure::Conflict {
                    detail: format!("pane {slot} changed since generation {generation} revision {revision}"),
                });
            }
            *generation = pane.generation;
            *revision = pane.revision;
        } else if let hl_extension::Request::PaneSemanticAction { slot, action } = &mut request {
            let topology = services.terminal.topology().ok().map(|topology| {
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                serde_json::to_vec(&topology).unwrap_or_default().hash(&mut hash);
                hash.finish()
            });
            let inventory = services.terminal.pane_inventory()?;
            let pane = inventory
                .panes
                .iter()
                .find(|pane| pane.slot == *slot)
                .ok_or_else(|| Failure::Absent {
                    detail: format!("pane {slot} is absent"),
                })?;
            let (_, observed_revision, observed_generation, _) = self.pane_state(services, pane, topology);
            if action.generation != observed_generation || action.revision != observed_revision {
                return Err(Failure::Conflict {
                    detail: format!(
                        "pane {slot} changed since generation {} revision {}",
                        action.generation, action.revision
                    ),
                });
            }
            // The peer observes the conversation's monotonic pane cursor;
            // the toolkit adapter authorizes its own stable provider/native
            // generation. Both checks happen in one dispatch.
            action.generation = pane.generation;
        }
        // A readable pane snapshot mints authority for a later byte write or
        // semantic action. Bracket it with canonical pane observations so a
        // changing UI cannot be returned with a newer cursor.
        let pane_read_generation = match &request {
            hl_extension::Request::TerminalReadPane { slot, .. } | hl_extension::Request::PaneSemanticRead { slot } => {
                Some(self.observe_pane_generation(services, slot)?)
            }
            _ => None,
        };
        let mut answer = self.session.dispatch(&request, services);
        if let Ok(reply) = &mut answer {
            self.attach_pane_cursors(reply, services);
            if let Some(before) = pane_read_generation {
                let changed_slot = match reply {
                    Reply::Text(text) if text.generation != before => Some(&text.slot),
                    Reply::Semantics(tree) if tree.generation != before => Some(&tree.slot),
                    _ => None,
                };
                if let Some(slot) = changed_slot {
                    answer = Err(Failure::Conflict {
                        detail: format!("pane {slot} changed while its readable snapshot was captured"),
                    });
                }
            }
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
                    self.subscriptions
                        .close(Topic::WorkspaceLifecycle, &mut self.channels, &mut self.outbox);
                }
                hl_extension::Request::EventUnsubscribe { topic } => {
                    // `Session::unfollow` stops publication authority; retire
                    // the transport route as part of the same acknowledged
                    // call so repeated topic use cannot leak channel budget or
                    // retain a coalesced snapshot after disposal.
                    self.subscriptions.close(*topic, &mut self.channels, &mut self.outbox);
                }
                _ => {}
            }
        }
        answer
    }

    fn observe_pane_generation(&mut self, services: &Services<'_>, slot: &str) -> Result<u64, Failure> {
        let topology = services.terminal.topology().ok().map(|topology| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_vec(&topology).unwrap_or_default().hash(&mut hash);
            hash.finish()
        });
        let inventory = services.terminal.pane_inventory()?;
        let pane = inventory
            .panes
            .iter()
            .find(|pane| pane.slot == slot)
            .ok_or_else(|| Failure::Absent {
                detail: format!("pane {slot} is absent"),
            })?;
        Ok(self.pane_state(services, pane, topology).2)
    }

    fn attach_pane_cursors(&mut self, reply: &mut Reply, services: &Services<'_>) {
        if !matches!(reply, Reply::Text(_) | Reply::Panes(_) | Reply::Semantics(_)) {
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
            Reply::Semantics(tree) => {
                if let Some(pane) = inventory.panes.iter().find(|pane| pane.slot == tree.slot) {
                    let (_, revision, generation, _) = self.pane_state(services, pane, topology);
                    tree.generation = generation;
                    tree.revision = revision;
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
            let payload = if message.superseded > 0 {
                match serde_json::from_slice::<Snapshot>(&message.payload) {
                    Ok(snapshot) => snapshot
                        .with_coalesced(message.superseded)
                        .payload()
                        .unwrap_or(message.payload),
                    Err(_) => message.payload,
                }
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
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use hl_extension::port::{
        ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
        PaneSummary, TabSummary, TerminalSurface, WorkspaceFiles,
    };
    use hl_extension::{
        Authority, Capability, Channels, ExtensionName, Failure, Flags, Frame, Grant, Hello, Kind, PROTOCOL,
        PreferenceValue, RelativePath, Reply, Request, Services, Transit, Wire, WorkspaceInfo, codec,
    };

    use super::{Compatibility, Conversation, Emission, Fault, Queue, Snapshot};
    use crate::extension::extension_state::StateBlob;
    use crate::extension::files::WorkspaceDirectory;

    /// What the adapters were actually asked for, so a refusal that still
    /// reached a service would be visible rather than silent.
    #[derive(Debug, Default)]
    struct Ledger {
        reached: Mutex<Vec<&'static str>>,
        image_pull_changes: Mutex<Vec<hl_extension::port::ImagePullChange>>,
        semantic_revision: AtomicU64,
        semantic_transition: AtomicU64,
        semantic_reads: AtomicU64,
        terminal_transition: AtomicU64,
        terminal_reads: AtomicU64,
    }

    impl Ledger {
        fn note(&self, what: &'static str) {
            self.reached.lock().expect("ledger").push(what);
        }

        fn reached(&self) -> Vec<&'static str> {
            self.reached.lock().expect("ledger").clone()
        }

        fn image_pull_changes(&self, changes: Vec<hl_extension::port::ImagePullChange>) {
            *self.image_pull_changes.lock().expect("image pull changes") = changes;
        }

        fn semantic_revision(&self, revision: u64) {
            self.semantic_revision.store(revision, Ordering::Release);
        }

        fn transition_semantics_during_read(&self, revision: u64) {
            self.semantic_revision.store(revision, Ordering::Release);
            self.semantic_transition.store(1, Ordering::Release);
        }

        fn transition_terminal_during_read(&self) {
            self.terminal_transition.store(1, Ordering::Release);
        }
    }

    #[test]
    fn credited_event_cannot_block_the_conversation_writer_without_bound() {
        let (ours, _peer) = UnixStream::pair().expect("socket pair");
        let authority = Authority::new(
            ExtensionName::new("stalled-reader").expect("name"),
            Grant::new([Capability::ContainerRead]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        conversation.arm_io_deadlines().expect("socket deadlines");
        let topic = hl_extension::Topic::Containers;
        conversation.session.follow(topic);
        conversation.route(topic).expect("subscription route");
        let channel = conversation.subscriptions.channel(topic).expect("routed channel");
        assert_eq!(
            conversation.subscriptions.emit(
                topic,
                vec![b'x'; Frame::PAYLOAD_LIMIT],
                &conversation.session,
                &mut conversation.channels,
                &mut conversation.outbox,
            ),
            Emission::Queued,
        );

        let started = Instant::now();
        let error = conversation
            .carry(topic)
            .expect_err("a peer that never reads must time out");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "credited event held the conversation worker for {:?}",
            started.elapsed()
        );
        assert!(matches!(error, Fault::Socket(_)), "unexpected write failure: {error:?}");
        assert_eq!(
            conversation.outbox.depth(channel),
            0,
            "failed write is not retained without bound"
        );
    }

    /// In-memory adapters: no container runtime and no window.
    struct Host {
        ledger: Arc<Ledger>,
    }

    impl hl_extension::NotificationSink for Host {
        fn publish(&self, _notification: &hl_extension::Notification) -> Result<(), HostError> {
            self.ledger.note("notifications.publish");
            Ok(())
        }
    }
    impl hl_extension::port::VolumeStore for Host {
        fn list(&self) -> Result<Vec<hl_extension::port::VolumeSummary>, HostError> {
            self.ledger.note("volumes.list");
            Ok(Vec::new())
        }
    }
    impl hl_extension::port::NetworkStore for Host {
        fn list(&self) -> Result<Vec<hl_extension::port::NetworkSummary>, HostError> {
            self.ledger.note("networks.list");
            Ok([("a", "database"), ("b", "unrelated")]
                .into_iter()
                .map(|(id, name)| hl_extension::port::NetworkSummary {
                    id: id.repeat(32),
                    name: name.into(),
                    driver: "bridge".into(),
                    scope: "local".into(),
                    kind: hl_extension::NetworkKind::Custom,
                    endpoints: None,
                })
                .collect())
        }

        fn inspect(&self, reference: &str) -> Result<hl_extension::port::NetworkSummary, HostError> {
            self.ledger.note("networks.inspect");
            let (id, name) = if reference == "database" || reference == "a".repeat(32) {
                ("a".repeat(32), "database")
            } else {
                ("b".repeat(32), "unrelated")
            };
            Ok(hl_extension::port::NetworkSummary {
                id,
                name: name.into(),
                driver: "bridge".into(),
                scope: "local".into(),
                kind: hl_extension::NetworkKind::Custom,
                endpoints: None,
            })
        }
    }

    impl ContainerInventory for Host {
        fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
            self.ledger.note("containers.list");
            Ok(vec![ContainerSummary {
                id: "c1".to_owned(),
                generation: 0,
                name: "api".to_owned(),
                image: "husklet/api:1".to_owned(),
                state: "running".to_owned(),
                created: 0,
                ports: Vec::new(),
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
                    id: "e".repeat(32),
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

        fn execution(&self, id: &str) -> Result<hl_extension::port::ExecutionSummary, HostError> {
            self.ledger.note("executions.inspect");
            Ok(hl_extension::port::ExecutionSummary {
                id: id.into(),
                container_id: "c1".into(),
                running: false,
                exit_code: 7,
                pid: 42,
                command: vec!["worker".into()],
                user: "root".into(),
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
                    timestamp_ms: 9,
                    stream: "stdout".into(),
                    bytes: b"row-42\n".to_vec(),
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
            self.ledger.note("containers.create");
            Ok(format!("id-{name}"))
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
    }

    impl ImageStore for Host {
        fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
            self.ledger.note("images.list");
            Ok(Vec::new())
        }

        fn pull_changes(&self, _owner: &str, after: u64) -> Vec<hl_extension::port::ImagePullChange> {
            self.ledger.note("images.pull_changes");
            self.ledger
                .image_pull_changes
                .lock()
                .expect("image pull changes")
                .iter()
                .filter(|change| change.sequence > after)
                .take(64)
                .cloned()
                .collect()
        }
    }

    impl TerminalSurface for Host {
        fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
            self.ledger.note("terminal.tabs");
            let semantic = self.ledger.semantic_revision.load(Ordering::Acquire) != 0;
            Ok(vec![TabSummary {
                id: "t1".to_owned(),
                title: "shell".to_owned(),
                pinned: false,
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

        fn pane_inventory(&self) -> Result<hl_extension::PaneInventory, HostError> {
            let semantic = self.ledger.semantic_revision.load(Ordering::Acquire) != 0;
            Ok(hl_extension::PaneInventory {
                panes: vec![hl_extension::InspectablePane {
                    slot: "s1".to_owned(),
                    generation: 0,
                    revision: 0,
                    kind: if semantic {
                        hl_extension::PaneKind::Native
                    } else {
                        hl_extension::PaneKind::Terminal
                    },
                    provider: None,
                    tab: Some("t1".to_owned()),
                    title: Some("shell".to_owned()),
                    focused: true,
                }],
                truncated: false,
            })
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
            let read = self.ledger.terminal_reads.fetch_add(1, Ordering::AcqRel);
            let transitioned = self.ledger.terminal_transition.load(Ordering::Acquire) != 0 && read >= 2;
            Ok(hl_extension::port::PaneText {
                slot: slot.to_owned(),
                generation: 0,
                revision: 0,
                columns: 120,
                rows: 40,
                lines: vec![if transitioned {
                    "changed while reading".to_owned()
                } else {
                    format!("at most {lines}")
                }],
                cursor_column: 12,
                cursor_row: 3,
                truncated: false,
            })
        }

        fn semantics(&self, slot: &str) -> Result<hl_extension::PaneSemanticTree, HostError> {
            self.ledger.note("terminal.semantics");
            let read = self.ledger.semantic_reads.fetch_add(1, Ordering::AcqRel);
            let mut revision = self.ledger.semantic_revision.load(Ordering::Acquire);
            if self.ledger.semantic_transition.load(Ordering::Acquire) != 0 && read >= 2 {
                revision = revision.saturating_add(1);
            }
            if revision == 0 {
                return Err(HostError::Unsupported("pane semantics are unavailable".into()));
            }
            Ok(hl_extension::PaneSemanticTree {
                slot: if revision == u64::MAX - 1 {
                    "another-pane".to_owned()
                } else {
                    slot.to_owned()
                },
                generation: 0,
                revision,
                root: hl_extension::SemanticNode {
                    id: 1,
                    role: "status".into(),
                    label: Some(if revision == u64::MAX {
                        "x".repeat(Frame::PAYLOAD_LIMIT)
                    } else {
                        "Lifecycle notice".into()
                    }),
                    value: Some(format!("revision {revision}")),
                    disabled: false,
                    destructive: false,
                    actions: Vec::new(),
                    children: if revision == u64::MAX - 2 {
                        vec![hl_extension::SemanticNode {
                            id: 1,
                            role: "button".into(),
                            label: Some("Ambiguous action".into()),
                            value: None,
                            disabled: false,
                            destructive: false,
                            actions: vec![hl_extension::SemanticActionKind::Invoke],
                            children: Vec::new(),
                        }]
                    } else {
                        Vec::new()
                    },
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

    impl hl_extension::port::WorkspaceControl for Host {
        fn inspect(&self, name: &str) -> Result<hl_extension::WorkspaceConfiguration, HostError> {
            self.ledger.note("workspace.inspect");
            Ok(hl_extension::WorkspaceConfiguration {
                generation: "0123456789abcdef0123456789abcdef".into(),
                configuration_revision: "abcdef0123456789abcdef0123456789".into(),
                name: name.into(),
                image: "alpine:3.20".into(),
                architecture: "arm64".into(),
                storage: None,
                shell: None,
                cpus: None,
                memory_mb: None,
                environment: vec![("DATABASE_PASSWORD".into(), "cycle19-socket-secret".into())],
                environment_redacted: false,
                mounts: Vec::new(),
                docker_socket: false,
                scrollback: None,
                vpn: None,
                execution_lifetime: "persisted".into(),
                terminal: hl_extension::WorkspaceTerminal::default(),
            })
        }
    }

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
                identity: None,
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
                identity: None,
            })
        }

        fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
            self.ledger.note("files.write");
            Ok(())
        }
    }

    impl hl_extension::port::ExtensionStore for Host {
        fn catalogue(&self) -> Result<hl_extension::port::ExtensionCatalogue, HostError> {
            self.ledger.note("extensions.catalogue");
            Ok(hl_extension::port::ExtensionCatalogue {
                entries: vec![hl_extension::port::ExtensionCatalogueEntry {
                    id: "storybook".into(),
                    title: "Component playground".into(),
                    description: "First-party components".into(),
                    version: "1.0.0".into(),
                    reference: "registry/storybook:latest".into(),
                    publisher: "Husklet".into(),
                    source: "husklet:first-party/storybook".into(),
                    publisher_verified: true,
                    protocol: hl_extension::PROTOCOL,
                    architectures: vec!["amd64".into()],
                }],
                complete: true,
            })
        }

        fn list(&self) -> Result<Vec<hl_extension::port::ExtensionSummary>, HostError> {
            self.ledger.note("extensions.list");
            Ok(vec![hl_extension::port::ExtensionSummary {
                name: "top".into(),
                image_digest: "sha256:manager".into(),
                status: "duty".into(),
                version: "1.0.0".into(),
                enabled: true,
                pane_providers: Vec::new(),
                granted: hl_extension::Grant::default(),
                images: hl_extension::ImageGrant::default(),
                containers: hl_extension::ContainerGrant::default(),
                networks: hl_extension::NetworkGrant::default(),
                volumes: hl_extension::VolumeGrant::default(),
                filesystem: hl_extension::FilesystemGrant::default(),
                workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
            }])
        }

        fn acquisition_status(&self, job: &str) -> Result<hl_extension::port::ExtensionAcquisitionStatus, HostError> {
            self.ledger.note("extensions.acquisition_status");
            Ok(hl_extension::port::ExtensionAcquisitionStatus {
                job: job.into(),
                reference: "registry/example:1".into(),
                revision: 7,
                state: "pulling".into(),
                progress: None,
                candidate: None,
                error: None,
            })
        }

        fn acquisition_cancel(&self, _job: &str, _revision: u64) -> Result<(), HostError> {
            self.ledger.note("extensions.acquisition_cancel");
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

    /// The grant every test starts from: read containers/extensions, and draw.
    fn authority() -> Authority {
        Authority::new(
            ExtensionName::new("sample").expect("name"),
            Grant::new([
                Capability::ContainerRead,
                Capability::ExtensionRead,
                Capability::ExtensionInstall,
                Capability::Interface,
                Capability::NotificationPublish,
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

    fn image_pull_host(ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new("image-observer").expect("name"),
                Grant::new([Capability::ImagePull]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        (theirs, served)
    }

    fn network_scoped_host(ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new("postgres").unwrap(),
                Grant::new([Capability::NetworkRead]),
                Vec::new(),
            );
            let mut conversation = Conversation::new_scoped(
                ours,
                authority,
                "dev",
                Queue::new(),
                hl_extension::ContainerGrant::default(),
                hl_extension::NetworkGrant {
                    selectors: vec![hl_extension::NetworkSelector::Name {
                        name: "database".into(),
                    }],
                    create: false,
                },
                hl_extension::VolumeGrant::default(),
                hl_extension::FilesystemGrant::default(),
                hl_extension::WorkspaceEnvironmentGrant::default(),
            )?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        (theirs, served)
    }

    #[test]
    fn exact_network_scope_filters_and_denies_over_the_real_unix_socket() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = network_scoped_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let listed = ask(&mut wire, &Request::NetworkList);
        assert!(matches!(codec::read_reply(&listed), Ok(Reply::Networks(inventory))
            if inventory.networks.len() == 1 && inventory.networks[0].name == "database"));
        let subscribed = ask(
            &mut wire,
            &Request::EventSubscribe {
                topic: hl_extension::Topic::Networks,
            },
        );
        assert_eq!(codec::read_reply(&subscribed), Ok(Reply::Done));
        let event = wire.receive().expect("scoped network snapshot");
        let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("typed snapshot");
        assert!(matches!(snapshot, Snapshot::Networks(inventory)
            if inventory.networks.len() == 1 && inventory.networks[0].name == "database"));
        let denied = ask(
            &mut wire,
            &Request::NetworkInspect {
                reference: "unrelated".into(),
            },
        );
        assert!(matches!(codec::read_failure(&denied), Ok(Failure::Denied { .. })));
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
        assert_eq!(ledger.reached(), ["networks.list", "networks.list"]);
    }

    #[test]
    fn full_state_quota_crosses_the_real_socket_and_the_conversation_survives() {
        let root = tempfile::tempdir().expect("state root");
        let state = StateBlob::new(root.path(), &ExtensionName::new("sample").unwrap()).expect("state store");
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new("sample").unwrap(),
                Grant::new([Capability::StateRead, Capability::StateWrite]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            let mut ports = services(&host);
            ports.state = &state;
            conversation.serve(&ports)
        });
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let contents = vec![255; 1024 * 1024];

        let initial = ask(&mut wire, &Request::StateRead);
        let Ok(Reply::State(initial)) = codec::read_reply(&initial) else {
            panic!("initial state must be readable");
        };
        let written = ask(
            &mut wire,
            &Request::StateWrite {
                observed: initial.identity,
                contents: contents.clone(),
            },
        );
        let Ok(Reply::Identity(identity)) = codec::read_reply(&written) else {
            panic!("full state quota must be writable");
        };
        let read = ask(&mut wire, &Request::StateRead);
        assert!(
            matches!(codec::read_reply(&read), Ok(Reply::State(state)) if state.identity == identity && state.contents == contents)
        );
        let cleared = ask(&mut wire, &Request::StateClear { observed: identity });
        assert!(matches!(codec::read_reply(&cleared), Ok(Reply::Done)));
        let empty = ask(&mut wire, &Request::StateRead);
        assert!(matches!(codec::read_reply(&empty), Ok(Reply::State(state)) if state.contents.is_empty()));

        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    fn preference_host(
        root: std::path::PathBuf,
        name: &'static str,
    ) -> (Wire<UnixStream>, JoinHandle<Result<(), Fault>>) {
        let state = StateBlob::new(&root, &ExtensionName::new(name).unwrap()).expect("preference store");
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new(name).unwrap(),
                Grant::new([Capability::PreferenceRead, Capability::PreferenceWrite]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            let mut ports = services(&host);
            ports.state = &state;
            conversation.serve(&ports)
        });
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        (wire, served)
    }

    #[test]
    fn preferences_are_isolated_bounded_cas_safe_and_durable_over_unix_framing() {
        let root = tempfile::tempdir().expect("preference root");
        let (mut alpha, served) = preference_host(root.path().to_path_buf(), "alpha");
        let initial = ask(&mut alpha, &Request::PreferenceRead);
        assert!(
            matches!(codec::read_reply(&initial), Ok(Reply::Preferences(preferences))
            if preferences.revision == 0 && preferences.entries.is_empty())
        );

        let set = ask(
            &mut alpha,
            &Request::PreferenceSet {
                observed: 0,
                key: "sidebar.width".into(),
                value: PreferenceValue::Number(320),
            },
        );
        assert_eq!(codec::read_reply(&set), Ok(Reply::Revision(1)));
        let stale = ask(
            &mut alpha,
            &Request::PreferenceSet {
                observed: 0,
                key: "sidebar.width".into(),
                value: PreferenceValue::Number(999),
            },
        );
        assert!(matches!(codec::read_failure(&stale), Ok(Failure::Conflict { .. })));
        let oversized = ask(
            &mut alpha,
            &Request::PreferenceSet {
                observed: 1,
                key: "label".into(),
                value: PreferenceValue::String("x".repeat(1025)),
            },
        );
        assert!(matches!(codec::read_failure(&oversized), Ok(Failure::Conflict { .. })));
        let invalid_key = ask(
            &mut alpha,
            &Request::PreferenceSet {
                observed: 1,
                key: "not/a/key".into(),
                value: PreferenceValue::Boolean(true),
            },
        );
        assert!(matches!(
            codec::read_failure(&invalid_key),
            Ok(Failure::Conflict { .. })
        ));

        let mut revision = 1;
        for index in 0..63 {
            let reply = ask(
                &mut alpha,
                &Request::PreferenceSet {
                    observed: revision,
                    key: format!("item.{index}"),
                    value: PreferenceValue::Boolean(index % 2 == 0),
                },
            );
            revision += 1;
            assert_eq!(codec::read_reply(&reply), Ok(Reply::Revision(revision)));
        }
        let full = ask(
            &mut alpha,
            &Request::PreferenceSet {
                observed: revision,
                key: "sixty-fifth".into(),
                value: PreferenceValue::Boolean(true),
            },
        );
        assert!(matches!(codec::read_failure(&full), Ok(Failure::Conflict { .. })));
        drop(alpha);
        assert_eq!(served.join().expect("alpha joined"), Ok(()));

        // A new conversation and a newly opened store model an application relaunch.
        let (mut relaunched, served) = preference_host(root.path().to_path_buf(), "alpha");
        let persisted = ask(&mut relaunched, &Request::PreferenceRead);
        assert!(
            matches!(codec::read_reply(&persisted), Ok(Reply::Preferences(preferences))
            if preferences.revision == revision
                && preferences.entries.len() == 64
                && preferences.entries.iter().any(|(key, value)|
                    key == "sidebar.width" && *value == PreferenceValue::Number(320)))
        );
        drop(relaunched);
        assert_eq!(served.join().expect("relaunch joined"), Ok(()));

        // The same workspace root and key under another authenticated extension is empty.
        let (mut beta, served) = preference_host(root.path().to_path_buf(), "beta");
        let isolated = ask(&mut beta, &Request::PreferenceRead);
        assert!(
            matches!(codec::read_reply(&isolated), Ok(Reply::Preferences(preferences))
            if preferences.revision == 0 && preferences.entries.is_empty())
        );
        drop(beta);
        assert_eq!(served.join().expect("beta joined"), Ok(()));
    }

    fn semantic_host(ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new("semantic-reader").expect("name"),
                Grant::new([Capability::PaneSemanticRead]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        (theirs, served)
    }

    fn terminal_host(ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new("terminal-agent").expect("name"),
                Grant::new([
                    Capability::ContainerRead,
                    Capability::TerminalOutput,
                    Capability::TerminalLayoutControl,
                ]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        (theirs, served)
    }

    fn filesystem_scoped_host(ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let roots = vec![
                RelativePath::new("src").unwrap(),
                RelativePath::new("workspace.toml").unwrap(),
            ];
            let authority = Authority::new(
                ExtensionName::new("sample").expect("name"),
                Grant::new([Capability::FilesystemRead, Capability::FilesystemWrite]),
                roots,
            );
            let mut conversation = Conversation::new_scoped(
                ours,
                authority,
                "dev",
                Queue::new(),
                hl_extension::ContainerGrant::default(),
                hl_extension::NetworkGrant::default(),
                hl_extension::VolumeGrant::default(),
                hl_extension::FilesystemGrant {
                    read: vec![
                        hl_extension::FilesystemSelector::Subtree {
                            subtree: RelativePath::new("src").unwrap(),
                        },
                        hl_extension::FilesystemSelector::Exact {
                            exact: RelativePath::new("workspace.toml").unwrap(),
                        },
                    ],
                    write: vec![hl_extension::FilesystemSelector::Exact {
                        exact: RelativePath::new("workspace.toml").unwrap(),
                    }],
                    ..hl_extension::FilesystemGrant::default()
                },
                hl_extension::WorkspaceEnvironmentGrant::default(),
            )?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        (theirs, served)
    }

    fn exact_write_host(
        ledger: Arc<Ledger>,
        root: std::path::PathBuf,
        exact: RelativePath,
    ) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let files = WorkspaceDirectory::new(root).expect("workspace directory");
            let authority = Authority::new(
                ExtensionName::new("sample").expect("name"),
                Grant::new([Capability::FilesystemWrite]),
                vec![exact.clone()],
            );
            let mut conversation = Conversation::new_scoped(
                ours,
                authority,
                "dev",
                Queue::new(),
                hl_extension::ContainerGrant::default(),
                hl_extension::NetworkGrant::default(),
                hl_extension::VolumeGrant::default(),
                hl_extension::FilesystemGrant {
                    write: vec![hl_extension::FilesystemSelector::Exact { exact }],
                    ..hl_extension::FilesystemGrant::default()
                },
                hl_extension::WorkspaceEnvironmentGrant::default(),
            )?;
            let mut ports = services(&host);
            ports.files = &files;
            conversation.greet()?;
            conversation.serve(&ports)
        });
        (theirs, served)
    }

    fn filesystem_journal_host(root: std::path::PathBuf) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host {
                ledger: Arc::new(Ledger::default()),
            };
            let files = WorkspaceDirectory::new(root).expect("workspace directory");
            let authority = Authority::new(
                ExtensionName::new("indexer").expect("name"),
                Grant::new([Capability::FilesystemRead]),
                vec![
                    RelativePath::new("docs").unwrap(),
                    RelativePath::new("README.md").unwrap(),
                ],
            );
            let mut conversation = Conversation::new_scoped(
                ours,
                authority,
                "dev",
                Queue::new(),
                hl_extension::ContainerGrant::default(),
                hl_extension::NetworkGrant::default(),
                hl_extension::VolumeGrant::default(),
                hl_extension::FilesystemGrant {
                    read: vec![
                        hl_extension::FilesystemSelector::Subtree {
                            subtree: RelativePath::new("docs").unwrap(),
                        },
                        hl_extension::FilesystemSelector::Exact {
                            exact: RelativePath::new("README.md").unwrap(),
                        },
                    ],
                    ..hl_extension::FilesystemGrant::default()
                },
                hl_extension::WorkspaceEnvironmentGrant::default(),
            )?;
            let mut ports = services(&host);
            ports.files = &files;
            conversation.greet()?;
            conversation.serve(&ports)
        });
        (theirs, served)
    }

    fn workspace_read_host(ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new("sample").expect("name"),
                Grant::new([Capability::WorkspaceRead]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        (theirs, served)
    }

    fn workspace_environment_host(ledger: Arc<Ledger>) -> (UnixStream, JoinHandle<Result<(), Fault>>) {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let served = std::thread::spawn(move || {
            let host = Host { ledger };
            let authority = Authority::new(
                ExtensionName::new("sample").unwrap(),
                Grant::new([Capability::WorkspaceRead, Capability::WorkspaceEnvironmentRead]),
                Vec::new(),
            );
            let grant = hl_extension::WorkspaceEnvironmentGrant {
                read: vec![hl_extension::WorkspaceEnvironmentSelector::Exact {
                    workspace: "dev".into(),
                    name: "DATABASE_PASSWORD".into(),
                }],
                write: Vec::new(),
            };
            let mut conversation = Conversation::new_scoped(
                ours,
                authority,
                "dev",
                Queue::new(),
                hl_extension::ContainerGrant::default(),
                hl_extension::NetworkGrant::default(),
                hl_extension::VolumeGrant::default(),
                hl_extension::FilesystemGrant::default(),
                grant,
            )?;
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
            name: "top".into(),
            image_digest: "sha256:observed".into(),
            status: "duty".into(),
            version: "1.0.0".into(),
            enabled: true,
            pane_providers: Vec::new(),
            granted: hl_extension::Grant::default(),
            images: hl_extension::ImageGrant::default(),
            containers: hl_extension::ContainerGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        }]);
        conversation.with_extension_events(events);

        let batch = conversation.drain_extension_events().expect("native extension change");
        assert_eq!(batch.inventory.expect("inventory")[0].name, "top");
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
        let job = super::super::acquisition::AcquisitionJob::test(7);
        let job_wire = job.wire();
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
                if change.job == job_wire && change.revision == 3 && change.state == "reading-manifest"
        ));
    }

    #[test]
    fn pane_addressed_pointer_metadata_crosses_the_credited_unix_frame() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let mut wire = Wire::new(theirs);
        let event_authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::WorkspaceEvents]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, event_authority, "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::WorkspaceEvents);
        let events = super::super::host::Events::default();
        events.observe(hl_extension::WorkspaceEvent::Pointer {
            phase: hl_extension::PointerPhase::Scroll,
            slot: "pane-7".into(),
            generation: 9,
            x: 12.5,
            y: 4.0,
            button: None,
            modifiers: vec!["shift".into()],
            delta_x: Some(-1.0),
            delta_y: Some(2.0),
        });
        conversation.with_events(events);
        let host = Host {
            ledger: Arc::new(Ledger::default()),
        };

        conversation.observe(&services(&host)).expect("pointer observed");

        let frame = wire.receive().expect("credited pointer event");
        let snapshot: Snapshot = serde_json::from_slice(&frame.payload).expect("typed snapshot");
        assert!(matches!(snapshot, Snapshot::WorkspaceEvents(batch)
            if matches!(&batch.events[0], hl_extension::WorkspaceEvent::Pointer {
                phase: hl_extension::PointerPhase::Scroll,
                slot,
                generation: 9,
                modifiers,
                delta_x: Some(dx),
                delta_y: Some(dy),
                ..
            } if slot == "pane-7" && modifiers == &["shift"] && *dx == -1.0 && *dy == 2.0)));
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
    fn catalogue_metadata_crosses_the_real_socket_without_becoming_install_authority() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(&mut wire, &Request::ExtensionCatalogue);
        assert!(matches!(
            codec::read_reply(&answer),
            Ok(Reply::ExtensionCatalogue(catalogue))
                if catalogue.complete
                    && catalogue.entries.len() == 1
                    && catalogue.entries[0].source == "husklet:first-party/storybook"
                    && catalogue.entries[0].reference == "registry/storybook:latest"
        ));
        assert_eq!(ledger.reached(), vec!["extensions.catalogue"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn invalid_acquisition_cancel_reaches_no_service_and_the_session_continues() {
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let host_ledger = Arc::clone(&ledger);
        let served = std::thread::spawn(move || {
            let host = Host { ledger: host_ledger };
            let authority = Authority::new(
                ExtensionName::new("sample").expect("name"),
                Grant::new([Capability::ExtensionInstall]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let invalid = ask(
            &mut wire,
            &Request::ExtensionAcquisitionCancel {
                job: String::new(),
                revision: 7,
            },
        );
        assert!(codec::is_failure(&invalid), "an invalid job identifier is refused");
        assert!(ledger.reached().is_empty(), "invalid cancellation reached the service");

        let valid = ask(&mut wire, &Request::ExtensionAcquisitionStatus { job: "job-1".into() });
        assert!(matches!(
            codec::read_reply(&valid),
            Ok(Reply::ExtensionAcquisition(status))
                if status.job == "job-1" && status.revision == 7 && status.state == "pulling"
        ));
        assert_eq!(ledger.reached(), vec!["extensions.acquisition_status"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn catalogue_without_extension_read_is_denied_before_the_service() {
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let host_ledger = Arc::clone(&ledger);
        let served = std::thread::spawn(move || {
            let host = Host { ledger: host_ledger };
            let authority = Authority::new(
                ExtensionName::new("sample").expect("name"),
                Grant::new([Capability::ContainerRead]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(&mut wire, &Request::ExtensionCatalogue);
        let Failure::Denied { capability, .. } = codec::read_failure(&answer).expect("denial") else {
            panic!("catalogue without read authority must be denied");
        };
        assert_eq!(capability, Capability::ExtensionRead.as_str());
        assert!(ledger.reached().is_empty(), "the catalogue service was not called");
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn invalid_catalogue_is_rejected_without_ending_the_session() {
        struct InvalidCatalogue(Arc<Ledger>);

        impl hl_extension::port::ExtensionStore for InvalidCatalogue {
            fn catalogue(&self) -> Result<hl_extension::port::ExtensionCatalogue, HostError> {
                self.0.note("extensions.catalogue");
                Ok(hl_extension::port::ExtensionCatalogue {
                    entries: vec![hl_extension::port::ExtensionCatalogueEntry {
                        id: String::new(),
                        title: "Invalid".into(),
                        description: "empty identifiers are ambiguous".into(),
                        version: "1.0.0".into(),
                        reference: "registry/invalid:latest".into(),
                        publisher: "Husklet".into(),
                        source: "test:invalid".into(),
                        publisher_verified: false,
                        protocol: hl_extension::PROTOCOL,
                        architectures: vec!["amd64".into()],
                    }],
                    complete: true,
                })
            }
        }

        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let host_ledger = Arc::clone(&ledger);
        let served = std::thread::spawn(move || {
            let host = Host {
                ledger: Arc::clone(&host_ledger),
            };
            let invalid = InvalidCatalogue(host_ledger);
            let mut supplied = services(&host);
            supplied.extensions = &invalid;
            let mut conversation = Conversation::new(ours, authority(), "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&supplied)
        });
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let invalid = ask(&mut wire, &Request::ExtensionCatalogue);
        assert!(codec::is_failure(&invalid), "invalid catalogue metadata is refused");
        assert!(
            matches!(codec::read_failure(&invalid), Ok(Failure::Failed { ref detail }) if detail.contains("metadata"))
        );
        let later = ask(&mut wire, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&later), Ok(Reply::Containers(_))));
        assert_eq!(ledger.reached(), vec!["extensions.catalogue", "containers.list"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn paged_execution_output_crosses_the_real_unix_socket_with_its_cursor() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(
            &mut wire,
            &Request::ExecutionOutput {
                id: "e".repeat(32),
                after: 41,
                limit: 16,
            },
        );
        assert!(matches!(codec::read_reply(&answer), Ok(Reply::ExecutionOutput(page))
            if page.next == 42 && !page.eof && !page.gap && page.entries[0].bytes == b"row-42\n"));
        assert_eq!(
            ledger.reached(),
            vec!["executions.inspect", "containers.list", "executions.output"]
        );
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn bounded_notification_crosses_the_real_unix_socket() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let answer = ask(
            &mut wire,
            &Request::NotificationPublish {
                notification: hl_extension::Notification {
                    id: "index".into(),
                    title: "Index ready".into(),
                    body: "One million rows indexed".into(),
                },
            },
        );
        assert_eq!(codec::read_reply(&answer), Ok(Reply::Done));
        assert_eq!(ledger.reached(), vec!["notifications.publish"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
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
                generation: 0,
                name: "api".to_owned(),
                image: "husklet/api:1".to_owned(),
                state: "running".to_owned(),
                created: 0,
                ports: Vec::new(),
            }])
        );
        assert_eq!(ledger.reached(), vec!["containers.list"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()), "a hangup is not a fault");
    }

    #[test]
    fn filesystem_verb_scopes_fail_closed_over_the_extension_socket() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = filesystem_scoped_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(
            &mut wire,
            &Request::FilesystemWrite {
                path: RelativePath::new("src/lib.rs").unwrap(),
                contents: b"no".to_vec(),
            },
        );
        assert!(codec::is_failure(&answer));
        assert!(matches!(codec::read_failure(&answer), Ok(Failure::Denied { .. })));
        let answer = ask(
            &mut wire,
            &Request::FilesystemRead {
                path: RelativePath::new("private.toml").unwrap(),
            },
        );
        assert!(codec::is_failure(&answer));
        assert!(matches!(codec::read_failure(&answer), Ok(Failure::Denied { .. })));
        let answer = ask(
            &mut wire,
            &Request::FilesystemRemove {
                path: RelativePath::new("workspace.toml").unwrap(),
            },
        );
        assert!(codec::is_failure(&answer));
        assert!(matches!(codec::read_failure(&answer), Ok(Failure::Denied { .. })));
        assert!(
            ledger.reached().is_empty(),
            "denied calls must not reach the filesystem service"
        );

        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn terminal_or_container_file_writes_cross_scoped_cursor_pages_over_a_real_socket() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("private")).unwrap();
        std::fs::write(root.join("docs/old.md"), b"old").unwrap();
        for index in 0..300 {
            std::fs::write(root.join(format!("docs/{index:03}.md")), b"stable").unwrap();
        }
        std::fs::write(root.join("README.md"), b"readme").unwrap();
        std::fs::write(root.join("private/secret"), b"secret").unwrap();
        let (theirs, served) = filesystem_journal_host(root.clone());
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let inventory = ask(&mut wire, &Request::FilesystemInventory);
        let Ok(Reply::FileInventory(inventory)) = codec::read_reply(&inventory) else { panic!("inventory") };
        let journal = inventory.journal;
        let baseline = ask(&mut wire, &Request::FilesystemChanges { observed: journal.clone(), after: 0, limit: 2 });
        assert!(
            matches!(codec::read_reply(&baseline), Ok(Reply::FileChanges(page)) if page.changes.is_empty() && !page.truncated)
        );
        let foreign_cursor = ask(&mut wire, &Request::FilesystemChanges { observed: journal.clone(), after: 999, limit: 2 });
        let Reply::FileChanges(foreign_cursor) = codec::read_reply(&foreign_cursor).unwrap() else {
            panic!("page")
        };
        assert!(foreign_cursor.truncated, "{foreign_cursor:?}");
        assert_eq!(foreign_cursor.next, foreign_cursor.current);

        let stale_journal = ask(
            &mut wire,
            &Request::FilesystemChanges {
                observed: "f".repeat(32),
                after: 0,
                limit: 2,
            },
        );
        let Reply::FileChanges(stale_journal) = codec::read_reply(&stale_journal).unwrap() else {
            panic!("page")
        };
        assert!(stale_journal.truncated);
        assert!(stale_journal.changes.is_empty());
        assert_eq!(stale_journal.journal, journal);
        let recovered = ask(
            &mut wire,
            &Request::FilesystemChanges {
                observed: stale_journal.journal,
                after: stale_journal.next,
                limit: 2,
            },
        );
        assert!(
            matches!(codec::read_reply(&recovered), Ok(Reply::FileChanges(page)) if !page.truncated),
            "the same framed session must recover after journal invalidation"
        );

        std::fs::write(root.join("docs/transient.md"), b"short lived").unwrap();
        std::fs::remove_file(root.join("docs/transient.md")).unwrap();
        let mut transient_seen = false;
        let mut transient_cursor = 0;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(10));
            let framed = ask(
                &mut wire,
                &Request::FilesystemChanges {
                    observed: journal.clone(),
                    after: transient_cursor,
                    limit: 256,
                },
            );
            let Reply::FileChanges(page) = codec::read_reply(&framed).unwrap() else {
                panic!("page")
            };
            transient_seen |= page.changes.iter().any(|change| {
                change.path.as_str() == "docs/transient.md" && change.kind == hl_extension::FileChangeKind::Invalidate
            });
            transient_cursor = page.next;
            if transient_seen {
                break;
            }
        }
        assert!(transient_seen, "a transient create-delete must leave an invalidation");

        std::fs::write(root.join("docs/old.md"), b"changed").unwrap();
        std::fs::write(root.join("docs/new.md"), b"new").unwrap();
        std::fs::write(root.join("docs/299.md"), b"changed beyond the old inventory prefix").unwrap();
        std::fs::remove_file(root.join("README.md")).unwrap();
        std::fs::write(root.join("private/secret"), b"not visible").unwrap();

        let first = ask(
            &mut wire,
            &Request::FilesystemChanges {
                observed: journal.clone(),
                after: transient_cursor,
                limit: 2,
            },
        );
        let Reply::FileChanges(first) = codec::read_reply(&first).unwrap() else {
            panic!("page")
        };
        assert_eq!(first.changes.len(), 2);
        assert!(first.more);
        let mut paths = first
            .changes
            .iter()
            .map(|change| change.path.to_string())
            .collect::<Vec<_>>();
        let mut cursor = first.next;
        let current = loop {
            let framed = ask(
                &mut wire,
                &Request::FilesystemChanges {
                    observed: journal.clone(),
                    after: cursor,
                    limit: 2,
                },
            );
            let Reply::FileChanges(page) = codec::read_reply(&framed).unwrap() else {
                panic!("page")
            };
            paths.extend(page.changes.iter().map(|change| change.path.to_string()));
            cursor = page.next;
            if !page.more {
                break page.current;
            }
        };
        assert_eq!(
            paths,
            vec!["README.md", "docs", "docs/299.md", "docs/new.md", "docs/old.md"]
        );
        assert_eq!(cursor, current);
        assert!(!paths.iter().any(|path| path.starts_with("private/")));

        drop(wire);
        assert_eq!(served.join().unwrap(), Ok(()));
    }

    #[test]
    fn exact_file_read_grant_cannot_enumerate_children_over_the_extension_socket() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = filesystem_scoped_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let listed = ask(
            &mut wire,
            &Request::FilesystemList {
                path: RelativePath::new("workspace.toml").unwrap(),
            },
        );
        assert!(matches!(
            codec::read_failure(&listed),
            Ok(Failure::Denied { capability, .. }) if capability == Capability::FilesystemRead.as_str()
        ));
        assert!(
            ledger.reached().is_empty(),
            "exact authority reached directory enumeration"
        );

        let stated = ask(
            &mut wire,
            &Request::FilesystemStat {
                path: RelativePath::new("workspace.toml").unwrap(),
            },
        );
        assert!(matches!(codec::read_reply(&stated), Ok(Reply::Entry(_))));
        assert_eq!(ledger.reached(), vec!["files.stat"]);

        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn observed_exact_file_write_cannot_quarantine_a_directory_over_the_extension_socket() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("state")).expect("directory");
        std::fs::write(root.join("state/secret.txt"), b"preserved").expect("nested file");
        let exact = RelativePath::new("state").expect("path");
        let files = WorkspaceDirectory::new(&root).expect("workspace directory");
        let observed = files
            .stat(&exact)
            .expect("directory identity")
            .identity
            .expect("stable identity");
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = exact_write_host(Arc::clone(&ledger), root.clone(), exact.clone());
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(
            &mut wire,
            &Request::FilesystemWriteObserved {
                path: exact,
                observed,
                contents: b"replacement".to_vec(),
            },
        );
        assert!(matches!(codec::read_failure(&answer), Ok(Failure::Conflict { .. })));
        assert_eq!(
            std::fs::read(root.join("state/secret.txt")).expect("nested file remains visible"),
            b"preserved"
        );
        assert!(root.join("state").is_dir(), "the authorized name remains a directory");
        assert!(
            std::fs::read_dir(&root).expect("workspace listing").all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-old-")),
            "a rejected exact-file write must not hide the directory under a quarantine name"
        );
        assert!(
            ledger.reached().is_empty(),
            "the real filesystem adapter replaced the fake service"
        );

        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn read_only_workspace_inspection_never_frames_environment_secrets() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = workspace_read_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let answer = ask(&mut wire, &Request::WorkspaceInspect { name: "dev".into() });
        assert!(
            !answer
                .payload
                .windows(b"cycle19-socket-secret".len())
                .any(|window| window == b"cycle19-socket-secret"),
            "the host secret entered the extension-facing frame"
        );
        let Reply::WorkspaceConfiguration(configuration) = codec::read_reply(&answer).expect("workspace reply") else {
            panic!("unexpected reply")
        };
        assert!(configuration.environment.is_empty());
        assert!(configuration.environment_redacted);
        assert!(!format!("{configuration:?}").contains("cycle19-socket-secret"));
        assert_eq!(ledger.reached(), vec!["workspace.inspect"]);

        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn exact_workspace_environment_consent_crosses_the_real_unix_socket() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = workspace_environment_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let answer = ask(&mut wire, &Request::WorkspaceInspect { name: "dev".into() });
        let Reply::WorkspaceConfiguration(configuration) = codec::read_reply(&answer).unwrap() else {
            panic!("unexpected reply")
        };
        assert_eq!(
            configuration.environment,
            vec![("DATABASE_PASSWORD".into(), "cycle19-socket-secret".into())]
        );
        assert!(!configuration.environment_redacted);
        drop(wire);
        assert_eq!(served.join().unwrap(), Ok(()));
    }

    #[test]
    fn a_ping_is_answered_without_disturbing_the_next_call() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let channel = hl_extension::ChannelId::new(17);
        let payload = b"client-heartbeat-41".to_vec();
        wire.send(&Frame::new(channel, Kind::Ping, payload.clone()))
            .expect("ping sent over the Unix socket");
        let pong = wire.receive().expect("bounded pong");
        assert_eq!(pong.kind, Kind::Pong);
        assert_eq!(pong.channel, channel, "the heartbeat retains its correlation channel");
        assert_eq!(
            pong.payload, payload,
            "the heartbeat retains its opaque correlation payload"
        );
        assert!(
            ledger.reached().is_empty(),
            "heartbeats never dispatch workspace authority"
        );

        let answer = ask(&mut wire, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&answer), Ok(Reply::Containers(_))));
        assert_eq!(ledger.reached(), vec!["containers.list"]);
        drop(wire);
        assert_eq!(
            served.join().expect("joined"),
            Ok(()),
            "a hangup after a pong stays clean"
        );
    }

    #[test]
    fn semantic_flag_violations_are_refused_before_authority() {
        for flag in [Flags::ERROR, Flags::COALESCED] {
            let ledger = Arc::new(Ledger::default());
            let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
            let mut wire = Wire::new(theirs);
            shake(&mut wire, PROTOCOL);
            let request = codec::request(&Request::ContainerList).expect("request").flagged(flag);
            wire.send(&request).expect("malformed request sent");

            let refusal = wire.receive().expect("flag refusal");
            assert!(codec::is_failure(&refusal));
            assert!(
                matches!(codec::read_failure(&refusal), Ok(Failure::Unsupported { call }) if call.contains("complete, unflagged request"))
            );
            assert!(ledger.reached().is_empty(), "malformed flags reached authority");
            drop(wire);
            assert_eq!(served.join().expect("joined"), Ok(()));
        }
    }

    #[test]
    fn oversized_semantic_adapter_reply_is_refused_without_breaking_the_unix_conversation() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = semantic_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        for (revision, expected) in [
            (u64::MAX, "invalid node"),
            (u64::MAX - 1, "requested slot"),
            (u64::MAX - 2, "duplicate node identities"),
        ] {
            ledger.semantic_revision(revision);
            let refused = ask(&mut wire, &Request::PaneSemanticRead { slot: "s1".to_owned() });
            assert!(matches!(
                codec::read_failure(&refused),
                Ok(Failure::Conflict { detail }) if detail.contains(expected)
            ));
            assert!(
                refused.payload.len() < 512,
                "a hostile adapter reply becomes a small typed refusal"
            );
        }

        ledger.semantic_revision(1);
        let recovered = ask(&mut wire, &Request::PaneSemanticRead { slot: "s1".to_owned() });
        assert!(matches!(codec::read_reply(&recovered), Ok(Reply::Semantics(tree)) if tree.slot == "s1"));

        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
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
    fn sixty_four_image_pull_changes_cross_two_credited_unix_pages_in_order() {
        let ledger = Arc::new(Ledger::default());
        ledger.image_pull_changes(
            (1..=64)
                .map(|sequence| hl_extension::port::ImagePullChange {
                    sequence,
                    job: format!("{sequence:032x}"),
                    revision: 1,
                    state: "pulling".into(),
                    coalesced: 0,
                })
                .collect(),
        );
        let (theirs, served) = image_pull_host(Arc::clone(&ledger));
        theirs
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("peer deadline");
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let answer = ask(
            &mut wire,
            &Request::EventSubscribe {
                topic: hl_extension::Topic::ImagePulls,
            },
        );
        assert_eq!(codec::read_reply(&answer), Ok(Reply::Done));

        let mut first = Vec::new();
        let mut channel = None;
        for _ in 0..Channels::CREDIT {
            let event = wire.receive().expect("first credited image pull page");
            channel = Some(event.channel);
            let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("typed image pull change");
            let Snapshot::ImagePulls(change) = snapshot else {
                panic!("unexpected snapshot");
            };
            first.push(change.sequence);
        }
        assert_eq!(first, (1..=32).collect::<Vec<_>>());

        wire.send(&Frame::new(
            channel.expect("event channel"),
            Kind::Credit,
            serde_json::to_vec(&Channels::CREDIT).expect("credit"),
        ))
        .expect("return first page credit");
        let mut second = Vec::new();
        for _ in 0..Channels::CREDIT {
            let event = wire.receive().expect("second credited image pull page");
            let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("typed image pull change");
            let Snapshot::ImagePulls(change) = snapshot else {
                panic!("unexpected snapshot");
            };
            second.push(change.sequence);
        }
        assert_eq!(second, (33..=64).collect::<Vec<_>>());
        assert!(
            ledger
                .reached()
                .iter()
                .filter(|call| **call == "images.pull_changes")
                .count()
                >= 2,
            "the second page came from a cursor read, not a retained flood"
        );
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn reconnect_after_subscribed_disconnect_starts_without_stale_observation() {
        let ledger = Arc::new(Ledger::default());
        let (first, first_served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut first = Wire::new(first);
        shake(&mut first, PROTOCOL);
        let answer = ask(
            &mut first,
            &Request::EventSubscribe {
                topic: hl_extension::Topic::Containers,
            },
        );
        assert_eq!(codec::read_reply(&answer).expect("subscription reply"), Reply::Done);
        assert_eq!(first.receive().expect("initial event").kind, Kind::Event);
        drop(first);
        assert_eq!(first_served.join().expect("first joined"), Ok(()));

        let reads_after_disconnect = ledger
            .reached()
            .iter()
            .filter(|call| **call == "containers.list")
            .count();
        let (second, second_served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut second = Wire::new(second);
        shake(&mut second, PROTOCOL);
        let answer = ask(&mut second, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&answer), Ok(Reply::Containers(_))));
        assert_eq!(
            ledger
                .reached()
                .iter()
                .filter(|call| **call == "containers.list")
                .count(),
            reads_after_disconnect + 1,
            "only the explicit call reached the container service"
        );
        drop(second);
        assert_eq!(second_served.join().expect("second joined"), Ok(()));
    }

    #[test]
    fn closing_an_event_channel_stops_observation_without_closing_calls() {
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
        let event = wire.receive().expect("initial event names its channel");
        assert_eq!(event.kind, Kind::Event);
        let reads_before_close = ledger
            .reached()
            .iter()
            .filter(|call| **call == "containers.list")
            .count();

        wire.send(&Frame::new(event.channel, Kind::Close, Vec::new()))
            .expect("channel close sent");
        assert_eq!(
            wire.receive(),
            Err(Transit::Pending),
            "a closed channel emits no more events"
        );
        let reads_after_close = ledger
            .reached()
            .iter()
            .filter(|call| **call == "containers.list")
            .count();
        assert_eq!(
            reads_after_close, reads_before_close,
            "a closed channel induces no service polling"
        );

        let answer = ask(&mut wire, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&answer), Ok(Reply::Containers(_))));
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
            matches!(snapshot, Snapshot::Executions(list) if list.executions[0].id == "e".repeat(32) && !list.executions[0].running)
        );
    }

    #[test]
    fn container_and_execution_events_are_filtered_by_recorded_resource_consent() {
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let mut conversation = Conversation::new_scoped(
            ours,
            authority(),
            "dev",
            Queue::new(),
            hl_extension::ContainerGrant {
                selectors: vec![hl_extension::ContainerSelector::Name { name: "other".into() }],
                create: false,
            },
            hl_extension::NetworkGrant::default(),
            hl_extension::VolumeGrant::default(),
            hl_extension::FilesystemGrant::default(),
            hl_extension::WorkspaceEnvironmentGrant::default(),
        )
        .expect("conversation");
        let host = Host { ledger };
        conversation.session.follow(hl_extension::Topic::Containers);
        conversation.observe(&services(&host)).unwrap();
        let mut wire = Wire::new(theirs);
        let event = wire.receive().expect("filtered container event");
        let snapshot: Snapshot = serde_json::from_slice(&event.payload).unwrap();
        assert!(matches!(snapshot, Snapshot::Containers(containers) if containers.is_empty()));

        conversation.session.follow(hl_extension::Topic::Executions);
        conversation.observe(&services(&host)).unwrap();
        let event = wire.receive().expect("filtered execution event");
        let snapshot: Snapshot = serde_json::from_slice(&event.payload).unwrap();
        assert!(matches!(snapshot, Snapshot::Executions(list) if list.executions.is_empty()));
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
            matches!(snapshot, Snapshot::Extensions(extensions) if extensions.len() == 1 && extensions[0].name == "top")
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
                generation: 0,
                name: "api".into(),
                image: "image".into(),
                state: "running".into(),
                created,
                ports: Vec::new(),
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
    fn framed_unsubscribe_retires_the_topic_channel_and_queued_snapshot() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        theirs
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("peer deadline");
        let mut conversation = Conversation::new(ours, authority(), "dev", Queue::new()).expect("conversation");
        let host = Host {
            ledger: Arc::new(Ledger::default()),
        };
        let ports = services(&host);
        let topic = hl_extension::Topic::Containers;
        let snapshot = Snapshot::Containers(vec![ContainerSummary {
            id: "c1".into(),
            generation: 0,
            name: "api".into(),
            image: "image".into(),
            state: "running".into(),
            created: 1,
            ports: Vec::new(),
        }]);
        let mut peer = Wire::new(theirs);

        let subscribe = codec::request(&Request::EventSubscribe { topic }).expect("subscribe request");
        conversation.exchange(&subscribe, &ports).expect("subscribe");
        assert_eq!(
            codec::read_reply(&peer.receive().expect("subscribe reply")).unwrap(),
            Reply::Done
        );
        conversation.publish(&snapshot).expect("first snapshot");
        let first = peer.receive().expect("first event");
        assert_eq!(first.kind, Kind::Event);
        let channel = first.channel;

        // Exhaust this route and leave a newer snapshot coalesced behind it.
        for created in 2..=hl_extension::Channels::CREDIT {
            let mut next = snapshot.clone();
            if let Snapshot::Containers(containers) = &mut next {
                containers[0].created = i64::from(created);
            }
            conversation.publish(&next).expect("credited snapshot");
            peer.receive().expect("credited event");
        }
        assert_eq!(conversation.publish(&snapshot), Ok(Emission::Superseded));
        assert_eq!(conversation.outbox.depth(channel), 1);

        let unsubscribe = codec::request(&Request::EventUnsubscribe { topic }).expect("unsubscribe request");
        conversation.exchange(&unsubscribe, &ports).expect("unsubscribe");
        assert_eq!(
            codec::read_reply(&peer.receive().expect("unsubscribe reply")).unwrap(),
            Reply::Done
        );
        assert_eq!(conversation.subscriptions.channel(topic), None);
        assert_eq!(
            conversation.channels.credit(channel),
            None,
            "channel budget is returned"
        );
        assert_eq!(
            conversation.outbox.depth(channel),
            0,
            "stale coalesced state is discarded"
        );

        let stale_credit = Frame::new(channel, Kind::Credit, serde_json::to_vec(&1_u32).unwrap());
        conversation
            .exchange(&stale_credit, &ports)
            .expect("stale credit ignored");
    }

    #[test]
    fn stalled_subscriptions_do_no_service_reads_and_resume_after_exact_credit() {
        let ledger = Arc::new(Ledger::default());
        let host = Host {
            ledger: Arc::clone(&ledger),
        };
        let (ours, _theirs) = UnixStream::pair().expect("socket pair");
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([
                Capability::ContainerRead,
                Capability::ImageRead,
                Capability::ImagePull,
                Capability::VolumeRead,
                Capability::NetworkRead,
                Capability::TerminalRead,
            ]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        let topics = [
            hl_extension::Topic::Containers,
            hl_extension::Topic::Executions,
            hl_extension::Topic::Images,
            hl_extension::Topic::ImagePulls,
            hl_extension::Topic::Volumes,
            hl_extension::Topic::Networks,
            hl_extension::Topic::Terminal,
        ];
        for topic in topics {
            conversation.session.follow(topic);
            conversation.route(topic).expect("subscription route");
            let channel = conversation.subscriptions.channel(topic).expect("routed channel");
            for _ in 0..hl_extension::Channels::CREDIT {
                assert_eq!(
                    conversation.channels.reserve(channel),
                    Ok(hl_extension::Permission::Send),
                    "every initial credit is available for {topic:?}",
                );
            }
            assert_eq!(conversation.channels.credit(channel), Some(0));
        }

        conversation.observe(&services(&host)).expect("stalled observation");
        assert!(
            ledger.reached().is_empty(),
            "zero credit must stop work before every subscribed service boundary"
        );

        let resumed = hl_extension::Topic::Containers;
        let channel = conversation.subscriptions.channel(resumed).expect("container channel");
        let credit = Frame::new(channel, Kind::Credit, serde_json::to_vec(&1_u32).expect("credit"));
        conversation
            .exchange(&credit, &services(&host))
            .expect("credit returned");
        conversation.observe(&services(&host)).expect("resumed observation");
        assert_eq!(
            ledger.reached(),
            vec!["containers.list"],
            "only the exact replenished topic resumes, from a fresh service read"
        );
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
    fn workspace_lifecycle_cursor_waits_for_each_credited_page() {
        let host = Host {
            ledger: Arc::new(Ledger::default()),
        };
        let lifecycle = LifecycleHost(
            (1..=64)
                .map(|revision| hl_extension::WorkspaceLifecycleChange {
                    workspace: format!("workspace-{revision}"),
                    action: hl_extension::WorkspaceLifecycleAction::Start,
                    revision,
                    coalesced: 0,
                })
                .collect(),
        );
        let (ours, peer) = UnixStream::pair().expect("socket pair");
        peer.set_read_timeout(Some(Duration::from_secs(2))).expect("timeout");
        let authority = Authority::new(
            ExtensionName::new("observer").expect("name"),
            Grant::new([Capability::WorkspaceRead]),
            Vec::new(),
        );
        let mut conversation = Conversation::new(ours, authority, "dev", Queue::new()).expect("conversation");
        conversation.session.follow(hl_extension::Topic::WorkspaceLifecycle);
        conversation.workspace_lifecycle_revision = Some(0);
        let mut ports = services(&host);
        ports.workspace_control = &lifecycle;
        let mut wire = Wire::new(peer);

        conversation.observe(&ports).expect("first lifecycle page");
        let mut channel = None;
        let mut first = Vec::new();
        for _ in 0..Channels::CREDIT {
            let event = wire.receive().expect("first credited lifecycle page");
            channel = Some(event.channel);
            let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("snapshot");
            let Snapshot::WorkspaceLifecycle(change) = snapshot else {
                panic!("unexpected snapshot");
            };
            first.push(change.revision);
        }
        assert_eq!(first, (1..=32).collect::<Vec<_>>());
        assert_eq!(conversation.workspace_lifecycle_revision, Some(32));

        let credit = Frame::new(
            channel.expect("event channel"),
            Kind::Credit,
            serde_json::to_vec(&Channels::CREDIT).expect("credit"),
        );
        conversation.exchange(&credit, &ports).expect("return page credit");
        conversation.observe(&ports).expect("second lifecycle page");
        let mut second = Vec::new();
        for _ in 0..Channels::CREDIT {
            let event = wire.receive().expect("second credited lifecycle page");
            let snapshot: Snapshot = serde_json::from_slice(&event.payload).expect("snapshot");
            let Snapshot::WorkspaceLifecycle(change) = snapshot else {
                panic!("unexpected snapshot");
            };
            second.push(change.revision);
        }
        assert_eq!(second, (33..=64).collect::<Vec<_>>());
        assert_eq!(conversation.workspace_lifecycle_revision, Some(64));
    }

    #[test]
    fn native_and_socket_mutations_reach_the_same_subscriber() {
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
        let mut observed = false;
        for _ in 0..256 {
            let frame = wire.receive().expect("event");
            let event: Snapshot = serde_json::from_slice(&frame.payload).expect("snapshot");
            if matches!(event, Snapshot::WorkspaceLifecycle(change)
                if change.workspace == name && change.action == hl_extension::WorkspaceLifecycleAction::Create)
            {
                observed = true;
                break;
            }
            let credit = Frame::new(frame.channel, Kind::Credit, serde_json::to_vec(&1_u32).expect("credit"));
            conversation.exchange(&credit, &ports).expect("return event credit");
        }
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
        let mut semantics = Reply::Semantics(host.semantics("s1").expect("host-local semantic tree"));
        conversation.attach_pane_cursors(&mut semantics, &services(&host));
        let Reply::Semantics(tree) = semantics else {
            panic!("semantic reply retained its kind");
        };
        assert_eq!(tree.slot, "s1");
        assert_eq!((tree.generation, tree.revision), (1, 1));
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
        assert!(!welcome.granted.holds(Capability::ContainerLifecycle));
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
    fn handshake_deadline_is_total_even_when_a_peer_trickles_bytes() {
        let deadline = Duration::from_millis(150);
        let (theirs, served) = host(deadline, Queue::new(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);
        codec::read_welcome(&wire.receive().expect("welcome")).expect("typed welcome");
        let hello = Hello {
            protocol: PROTOCOL,
            name: ExtensionName::new("slow_peer").expect("name"),
            features: Vec::new(),
        };
        let bytes = codec::hello(&hello).expect("hello").encode().expect("framed hello");
        let mut peer = wire.into_stream();
        let trickle = std::thread::spawn(move || {
            for byte in bytes {
                if peer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });

        let started = Instant::now();
        let fault = served
            .join()
            .expect("conversation thread")
            .expect_err("slow handshake dropped");
        let elapsed = started.elapsed();
        assert_eq!(fault, Fault::Handshake(Compatibility::Unknown));
        assert!(elapsed >= deadline, "deadline fired early after {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(400),
            "byte trickle extended the total handshake deadline to {elapsed:?}"
        );
        trickle.join().expect("trickle thread");
    }

    #[test]
    fn a_peer_cannot_hold_the_only_conversation_with_an_unfinished_frame() {
        let deadline = Duration::from_millis(400);
        let (theirs, served) = host(deadline, Queue::new(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let encoded = codec::request(&Request::ContainerList)
            .expect("request")
            .encode()
            .expect("frame");
        let mut stream = wire.into_stream();
        let started = Instant::now();
        stream.write_all(&encoded[..5]).expect("partial frame header");

        let fault = served
            .join()
            .expect("conversation thread")
            .expect_err("partial peer released");
        assert!(matches!(fault, Fault::Malformed(ref detail) if detail.contains("unfinished frame")));
        assert!(
            started.elapsed() >= deadline,
            "an ordinary observation tick is not mistaken for a stalled frame"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the partial-frame deadline remains bounded"
        );
    }

    #[test]
    fn request_deadline_is_total_even_when_a_peer_trickles_bytes() {
        let deadline = Duration::from_millis(150);
        let (theirs, served) = host(deadline, Queue::new(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let bytes = codec::request(&Request::ContainerList)
            .expect("request")
            .encode()
            .expect("framed request");
        let mut peer = wire.into_stream();
        let trickle = std::thread::spawn(move || {
            for byte in bytes {
                if peer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });

        let started = Instant::now();
        let fault = served
            .join()
            .expect("conversation thread")
            .expect_err("slow request dropped");
        let elapsed = started.elapsed();
        assert!(matches!(fault, Fault::Malformed(ref detail) if detail.contains("unfinished frame")));
        assert!(elapsed >= deadline, "deadline fired early after {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(400),
            "byte trickle extended the total request deadline to {elapsed:?}"
        );
        trickle.join().expect("trickle thread");
    }

    #[test]
    fn an_interface_frame_is_collected_for_the_window_rather_than_applied() {
        let queue = Queue::new();
        let (theirs, served) = host(Duration::from_secs(5), queue.clone(), Arc::new(Ledger::default()));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let drawn = ask(
            &mut wire,
            &Request::InterfaceRenderAt {
                slot: String::new(),
                frame: hl_gui::Frame::new(1),
            },
        );

        assert_eq!(codec::read_reply(&drawn).expect("a reply"), Reply::Done);
        let collected = queue.collect();
        assert_eq!(collected.frames.len(), 1, "the frame is held for the window");
        assert_eq!(collected.frames[0].slot, "");
        assert_eq!(collected.frames[0].frame.sequence, 1);
        assert!(queue.is_empty(), "collecting empties the queue");
        drop(wire);
        let _ = served.join().expect("joined");
    }

    #[test]
    fn a_coalesced_control_close_revokes_later_interface_frames() {
        let queue = Queue::new();
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), queue.clone(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let before = codec::request(&Request::InterfaceRenderAt {
            slot: String::new(),
            frame: hl_gui::Frame::new(1),
        })
        .expect("first render frame")
        .encode()
        .expect("first render bytes");
        let close = Frame::control(Kind::Close, Vec::new()).encode().expect("close bytes");
        let after = codec::request(&Request::InterfaceRenderAt {
            slot: String::new(),
            frame: hl_gui::Frame::new(2),
        })
        .expect("stale render frame")
        .encode()
        .expect("stale render bytes");
        let mut coalesced = Vec::with_capacity(before.len() + close.len() + after.len());
        coalesced.extend_from_slice(&before);
        coalesced.extend_from_slice(&close);
        coalesced.extend_from_slice(&after);
        let mut peer = wire.into_stream();
        peer.write_all(&coalesced)
            .expect("one kernel write carries render, close, and stale render");
        peer.shutdown(std::net::Shutdown::Write)
            .expect("the complete coalesced stream has been sent");

        assert_eq!(served.join().expect("joined"), Ok(()));
        let collected = queue.collect();
        assert_eq!(collected.frames.len(), 1, "only pre-close GUI work is published");
        assert_eq!(collected.frames[0].frame.sequence, 1);
        assert!(
            ledger.reached().is_empty(),
            "interface rendering reaches no workspace authority"
        );
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
    fn an_invalid_source_schema_never_reaches_the_window_queue() {
        let queue = Queue::new();
        let duplicate = hl_gui::Column::new("same", "Name");
        let mutation = hl_extension::SurfaceMutation {
            slot: "table-pane".into(),
            mutation: hl_gui::SourceMutation::Open {
                source: hl_gui::SourceId::new(1),
                columns: vec![duplicate.clone(), duplicate],
            },
        };
        let refusal = queue.deposit(Vec::new(), vec![mutation]);
        assert!(matches!(refusal, Err(Fault::Malformed(ref detail)) if detail.contains("invalid table schema")));
        assert!(queue.is_empty(), "invalid schema allocates no GUI work");
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

        let answer = ask(
            &mut wire,
            &Request::ContainerStop {
                id: "c1".to_owned(),
                generation: 4,
            },
        );

        assert!(codec::is_failure(&answer), "a refusal is reported as one");
        let Failure::Denied { capability, .. } = codec::read_failure(&answer).expect("a failure") else {
            panic!("an ungranted call is a denial");
        };
        assert_eq!(capability, Capability::ContainerLifecycle.as_str());
        assert!(ledger.reached().is_empty(), "nothing may be reached before the check");
        drop(wire);
        let _ = served.join().expect("joined");
    }

    #[test]
    fn invalid_exec_environment_never_echoes_its_secret_and_the_session_recovers() {
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let host_ledger = Arc::clone(&ledger);
        let served = std::thread::spawn(move || {
            let host = Host { ledger: host_ledger };
            let authority = Authority::new(
                ExtensionName::new("sample").expect("name"),
                Grant::new([Capability::ContainerRead, Capability::ContainerExecute]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let secret = "sentinel-password-never-in-a-socket-reply";

        let failure = ask(
            &mut wire,
            &Request::ContainerExec {
                id: "c".repeat(64),
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
            },
        );
        assert!(codec::is_failure(&failure), "duplicate environment names are refused");
        assert!(
            !failure
                .payload
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "the failure frame echoed an environment secret"
        );
        assert!(ledger.reached().is_empty(), "invalid environment reached a service");

        let later = ask(&mut wire, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&later), Ok(Reply::Containers(_))));
        assert_eq!(ledger.reached(), vec!["containers.list"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn terminal_read_refuses_to_pair_changed_text_with_fresh_write_authority() {
        let ledger = Arc::new(Ledger::default());
        ledger.transition_terminal_during_read();
        let (theirs, served) = terminal_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        // The fixture changes after the canonical observation and requested
        // text read, but before the authority cursor would be attached.
        let read = ask(
            &mut wire,
            &Request::TerminalReadPane {
                slot: "s1".into(),
                lines: Some(199),
            },
        );
        assert!(matches!(codec::read_failure(&read), Ok(Failure::Conflict { .. })));

        let later = ask(&mut wire, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&later), Ok(Reply::Containers(_))));
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn semantic_read_refuses_stale_nodes_with_a_newer_action_cursor() {
        let ledger = Arc::new(Ledger::default());
        ledger.transition_semantics_during_read(1);
        let (theirs, served) = semantic_host(Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let raced = ask(&mut wire, &Request::PaneSemanticRead { slot: "s1".into() });
        assert!(matches!(codec::read_failure(&raced), Ok(Failure::Conflict { .. })));

        let stable = ask(&mut wire, &Request::PaneSemanticRead { slot: "s1".into() });
        assert!(matches!(codec::read_reply(&stable), Ok(Reply::Semantics(_))));
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
    }

    #[test]
    fn denied_terminal_write_never_inspects_the_pane_or_echoes_input() {
        let ledger = Arc::new(Ledger::default());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let host_ledger = Arc::clone(&ledger);
        let served = std::thread::spawn(move || {
            let host = Host { ledger: host_ledger };
            let authority = Authority::new(
                ExtensionName::new("sample").expect("name"),
                Grant::new([Capability::ContainerRead]),
                Vec::new(),
            );
            let mut conversation = Conversation::new(ours, authority, "dev", Queue::new())?;
            conversation.greet()?;
            conversation.serve(&services(&host))
        });
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);
        let secret = b"sentinel-terminal-input-never-in-a-failure";

        let denied = ask(
            &mut wire,
            &Request::TerminalWritePane {
                slot: "s1".into(),
                generation: 1,
                revision: 2,
                contents: secret.to_vec(),
            },
        );
        assert!(matches!(
            codec::read_failure(&denied),
            Ok(Failure::Denied { capability, .. }) if capability == Capability::TerminalInput.as_str()
        ));
        assert!(!denied.payload.windows(secret.len()).any(|window| window == secret));
        assert!(ledger.reached().is_empty(), "denied input reached terminal inspection");
        let later = ask(&mut wire, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&later), Ok(Reply::Containers(_))));
        assert_eq!(ledger.reached(), vec!["containers.list"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
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
    fn an_unfinished_request_frame_never_dispatches_or_steals_the_next_reply() {
        let ledger = Arc::new(Ledger::default());
        let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
        let mut wire = Wire::new(theirs);
        shake(&mut wire, PROTOCOL);

        let mut unfinished = codec::request(&Request::ContainerList).expect("encoded request");
        unfinished.flags = hl_extension::Flags::none();
        wire.send(&unfinished).expect("unfinished frame sent over Unix socket");
        let refused = wire.receive().expect("unfinished request is answered");
        assert!(codec::is_failure(&refused));
        assert!(
            ledger.reached().is_empty(),
            "an unfinished logical request never reaches authority"
        );

        let answered = ask(&mut wire, &Request::ContainerList);
        assert!(matches!(codec::read_reply(&answered), Ok(Reply::Containers(_))));
        assert_eq!(ledger.reached(), vec!["containers.list"]);
        drop(wire);
        assert_eq!(served.join().expect("joined"), Ok(()));
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
    fn peer_only_frame_kinds_fail_closed_instead_of_being_ignored() {
        for kind in [Kind::Response, Kind::Event, Kind::Open, Kind::Reset, Kind::Pong] {
            let ledger = Arc::new(Ledger::default());
            let (theirs, served) = host(Duration::from_secs(5), Queue::new(), Arc::clone(&ledger));
            let mut wire = Wire::new(theirs);
            shake(&mut wire, PROTOCOL);
            wire.send(&Frame::new(
                hl_extension::ChannelId::new(2),
                kind,
                serde_json::to_vec(&serde_json::json!({})).unwrap(),
            ))
            .expect("illegal directional frame sent");
            let deadline = Instant::now() + Duration::from_millis(200);
            while !served.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(served.is_finished(), "unexpected {kind:?} must close promptly");
            let fault = served.join().expect("joined").expect_err("illegal frame must close");
            assert!(
                matches!(fault, Fault::Malformed(detail) if detail.contains(&format!("unexpected {kind:?} frame"))),
                "{kind:?} was not classified as a malformed directional frame"
            );
            assert!(ledger.reached().is_empty(), "{kind:?} never reaches authority");
        }
    }

    #[test]
    fn a_transport_failure_is_not_reported_as_a_malformed_peer() {
        assert_eq!(
            super::fault(Transit::Io("broken pipe".to_owned())),
            Fault::Socket("broken pipe".to_owned())
        );
    }
}
