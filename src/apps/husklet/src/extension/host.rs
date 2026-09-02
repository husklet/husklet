//! One installed extension, brought up and kept talking to a page.
//!
//! Every part this drives already exists: [`Records`] holds what a person
//! consented to, [`Sidecar`] owns the container, [`Listener`] owns the socket,
//! [`Conversation`] owns the protocol, and [`Installation`] owns the restart
//! policy. This module is the orchestration between them and nothing else, so
//! a change of policy belongs where the policy lives rather than here.
//!
//! The page is on the other side of two channels: [`Report`]s go out to
//! whatever draws, [`Order`]s come back in from whatever was interacted with.
//! Channels rather than a toolkit handle, because the extension is served on a
//! thread of its own and the window may only be touched on the main loop. No
//! toolkit type appears below, which is what lets the whole lifecycle be
//! exercised over a socket pair with no display and no container daemon.

use std::collections::VecDeque;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hl_extension::{Authority, ChannelId, Disposition, Installation, Manifest, Record};

mod voice;
mod workspace;

use super::conversation::{Conversation, Queue};
use super::sidecar::SidecarSpec;
use super::Listener;
use crate::config::WorkspaceConfig;
use voice::{speak, Voice};

pub use workspace::Workspace;

/// The channel host-sent interface events ride on.
///
/// The same channel the reference producer already answers row requests on, so
/// an extension written against that reference needs nothing new to be hosted
/// here.
pub const EVENTS: ChannelId = ChannelId::new(3);

/// How often the driver looks at its queue and its orders.
///
/// The page applies what it is given on a 100 ms tick, so anything faster than
/// this would only queue work the window cannot show; anything slower would be
/// visible as lag on a click.
const POLL: Duration = Duration::from_millis(20);

/// What is shown when the workspace has no extension installed.
///
/// An absence is stated rather than left blank: a page that renders nothing and
/// says nothing reads as a broken extension, which is the one thing it is not.
pub const VACANCY: &str = "no extension is installed in this workspace";

/// Something the host has for the page to apply.
///
/// Mirrors what the page's own queue carries. It is restated here because the
/// page lives in the application binary and this module lives in the library,
/// so the two cannot share a type; the binary maps one onto the other where it
/// joins them.
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// A description of what to draw.
    Frame(hl_gui::Frame),
    /// A change to a windowed source the extension's tables draw from.
    Source(hl_gui::SourceMutation),
    /// The extension is not speaking any more, and why.
    Loss(String),
    /// The restart policy has latched a crash loop. Structured rather than
    /// parsed from `Loss`, so settings never guesses state from prose.
    Fault { restarts: u32 },
}

/// Where the page sends what a person did.
#[derive(Clone, Debug, PartialEq)]
pub enum Order {
    /// Interaction reported by the surface, including a table asking for a
    /// window of rows.
    Interaction(hl_gui::Event),
    /// A terminal pane selected one of this extension's named providers.
    PaneProvider(hl_extension::PaneSelection),
    /// Start the stopped extension again.
    Retry,
}

/// A non-blocking, bounded bridge from a workspace window to its extension.
#[derive(Clone, Default)]
pub struct Events {
    inner: Arc<Mutex<EventQueue>>,
}

#[derive(Default)]
struct EventQueue {
    pending: VecDeque<hl_extension::WorkspaceEvent>,
    dropped: u64,
}

impl Events {
    pub const LIMIT: usize = 64;

    /// Records activity without ever waiting for the GTK main loop.
    pub fn observe(&self, event: hl_extension::WorkspaceEvent) {
        let Ok(mut queue) = self.inner.try_lock() else { return };
        if matches!(
            event,
            hl_extension::WorkspaceEvent::Pointer {
                phase: hl_extension::PointerPhase::Move,
                ..
            }
        ) && matches!(
            queue.pending.back(),
            Some(hl_extension::WorkspaceEvent::Pointer {
                phase: hl_extension::PointerPhase::Move,
                ..
            })
        ) {
            queue.pending.pop_back();
        } else if queue.pending.len() == Self::LIMIT {
            queue.pending.pop_front();
            queue.dropped = queue.dropped.saturating_add(1);
        }
        queue.pending.push_back(event);
    }

    pub(crate) fn drain(&self) -> Option<hl_extension::WorkspaceEventBatch> {
        let Ok(mut queue) = self.inner.try_lock() else {
            return None;
        };
        if queue.pending.is_empty() && queue.dropped == 0 {
            return None;
        }
        Some(hl_extension::WorkspaceEventBatch {
            events: queue.pending.drain(..).collect(),
            dropped: std::mem::take(&mut queue.dropped),
        })
    }
}

#[cfg(test)]
mod event_buffer_tests {
    use super::Events;
    use hl_extension::{PointerPhase, WorkspaceEvent};

    fn motion(x: f64) -> WorkspaceEvent {
        WorkspaceEvent::Pointer {
            phase: PointerPhase::Move,
            x,
            y: 1.0,
            button: None,
        }
    }

    #[test]
    fn pointer_motion_is_coalesced_before_it_reaches_the_wire() {
        let events = Events::default();
        events.observe(motion(1.0));
        events.observe(motion(2.0));
        let batch = events.drain().unwrap();
        assert_eq!(batch.events, vec![motion(2.0)]);
        assert_eq!(batch.dropped, 0);
    }

    #[test]
    fn overload_is_bounded_and_reported() {
        let events = Events::default();
        for index in 0..Events::LIMIT + 7 {
            events.observe(WorkspaceEvent::Focus { active: index % 2 == 0 });
        }
        let batch = events.drain().unwrap();
        assert_eq!(batch.events.len(), Events::LIMIT);
        assert_eq!(batch.dropped, 7);
    }
}

/// Where the hosted extension stands.
///
/// Reported rather than inferred from the reports, because a page that has
/// received nothing yet has to tell "still starting" from "nothing installed",
/// and only one of those is worth a person's attention.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Standing {
    /// The installation has not been read yet.
    Search,
    /// The workspace has no extension to host.
    Vacancy,
    /// A conversation is open.
    Duty,
    /// The extension stopped, with the reason a person was shown.
    Loss(String),
}

/// Where a host sends what it collected.
///
/// A closure rather than a channel so the binary can post straight into the
/// page's own queue without a second hop.
pub type Audience = Box<dyn Fn(Report) + Send + 'static>;

/// Everything needed to bring one extension up, resolved before anything runs.
#[derive(Clone)]
pub struct Plan {
    /// What the person consented to, as it was written down.
    pub record: Record,
    /// The manifest the container is described from.
    pub manifest: Manifest,
    /// The container this extension gets.
    pub spec: SidecarSpec,
    /// The workspace the extension is told it is in.
    pub workspace: String,
}

impl Plan {
    /// The authority one conversation is served under.
    ///
    /// Built from the record's grant and never from the manifest's request, so
    /// an image that restates a wider request cannot widen what is running.
    #[must_use]
    pub fn authority(&self) -> Authority {
        Authority::new(
            self.record.name.clone(),
            self.record.granted.clone(),
            self.manifest.filesystem_roots.clone(),
        )
    }
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Plan")
            .field("extension", &self.record.name)
            .field("container", &self.spec.container())
            .finish_non_exhaustive()
    }
}

/// What a host takes from the workspace it runs in.
///
/// A trait because the container daemon is the one part of the lifecycle that
/// cannot be reached from a test: [`Workspace`] is the real supply, and the
/// suite below drives the same host over a socket pair with none of it.
pub trait Supply: Send + Sync + 'static {
    /// The extension this host serves, or `None` when the workspace has none.
    ///
    /// # Errors
    /// Returns why the installation could not be read.
    fn plan(&self) -> Result<Option<Plan>, String>;

    /// Brings the extension's container to the state the plan describes.
    ///
    /// # Errors
    /// Returns why the container could not be brought up.
    fn ensure(&self, plan: &Plan) -> Result<(), String>;

    /// Serves one accepted conversation until it ends.
    ///
    /// The ports are the supply's own, so the host never holds a service an
    /// extension might reach.
    ///
    /// # Errors
    /// Returns why the conversation ended early.
    fn attend(&self, plan: &Plan, conversation: &mut Conversation) -> Result<(), String>;

    /// Takes the extension's container down.
    fn halt(&self, plan: &Plan);
}

/// Why a host could not be shut down cleanly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Overrun {
    /// The driver had not finished when the deadline passed. It is joined
    /// anyway, because a detached thread holding an extension's socket is worse
    /// than a slow shutdown; this says the wait ran long.
    Deadline(Duration),
}

impl std::fmt::Display for Overrun {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Deadline(deadline) = self;
        write!(
            formatter,
            "an extension host was still running {} ms after it was asked to stop",
            deadline.as_millis()
        )
    }
}

impl std::error::Error for Overrun {}

/// One installed extension, hosted for one workspace.
pub struct Host {
    orders: mpsc::Sender<Order>,
    standing: Arc<Mutex<Standing>>,
    stop: Arc<AtomicBool>,
    ended: mpsc::Receiver<()>,
    driver: Option<JoinHandle<()>>,
}

impl Host {
    /// How long [`Host::close`] waits for the driver to finish.
    pub const DEADLINE: Duration = Duration::from_secs(10);

    /// Starts hosting whatever `supply` offers.
    ///
    /// Returns immediately: reading the installation, reaching the container
    /// daemon, and binding the socket all happen on the driver thread, so a
    /// caller on a main loop is never held by any of them.
    #[must_use]
    pub fn open<S: Supply>(supply: S, audience: Audience) -> Self {
        let (orders, inbox) = mpsc::channel();
        let (finished, ended) = mpsc::channel();
        let standing = Arc::new(Mutex::new(Standing::Search));
        let stop = Arc::new(AtomicBool::new(false));
        let hall = Hall {
            audience,
            inbox,
            standing: Arc::clone(&standing),
            stop: Arc::clone(&stop),
        };
        let driver = std::thread::spawn(move || {
            drive(&Arc::new(supply), &hall);
            let _ = finished.send(());
        });
        Self {
            orders,
            standing,
            stop,
            ended,
            driver: Some(driver),
        }
    }

    /// Starts hosting the extension one workspace has installed.
    #[must_use]
    pub fn workspace(workspace: &WorkspaceConfig, audience: Audience) -> Self {
        Self::open(Workspace::new(workspace), audience)
    }

    /// Where the extension stands right now.
    #[must_use]
    pub fn standing(&self) -> Standing {
        self.standing.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Hands the host one order from the page.
    ///
    /// Never blocks and never fails: an order for a driver that has already
    /// finished is dropped, because there is nothing left to act on it.
    pub fn accept(&self, order: Order) {
        let _ = self.orders.send(order);
    }

    /// Ends the host: stops the driver, closes the socket, stops the sidecar,
    /// and joins.
    ///
    /// # Errors
    /// Returns `Overrun::Deadline` when the driver outlasted the deadline. It is
    /// joined either way, so no thread outlives this call.
    pub fn close(mut self) -> Result<(), Overrun> {
        self.shutdown()
    }

    /// The whole of shutdown, written once so dropping a host does exactly what
    /// closing it does.
    fn shutdown(&mut self) -> Result<(), Overrun> {
        let Some(driver) = self.driver.take() else {
            return Ok(());
        };
        self.stop.store(true, Ordering::Release);
        let started = Instant::now();
        let late = self.ended.recv_timeout(Self::DEADLINE).is_err() && started.elapsed() >= Self::DEADLINE;
        let _ = driver.join();
        if late {
            return Err(Overrun::Deadline(Self::DEADLINE));
        }
        Ok(())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Dropping cannot report, and a driver that outran its deadline is the
        // one thing shutdown does not resolve on its own.
        if let Err(fault) = self.shutdown() {
            hl_log::hl_error!(hl_log::tag::RUNTIME, "{fault}");
        }
    }
}

impl std::fmt::Debug for Host {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Host")
            .field("standing", &self.standing())
            .finish_non_exhaustive()
    }
}

/// What the driver thread owns: where reports go, where orders arrive, and the
/// two flags the owning [`Host`] reads and writes.
struct Hall {
    audience: Audience,
    inbox: mpsc::Receiver<Order>,
    standing: Arc<Mutex<Standing>>,
    stop: Arc<AtomicBool>,
}

impl Hall {
    /// Whether the host has been asked to stop.
    fn halted(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Hands one report to whatever draws.
    fn deliver(&self, report: Report) {
        (self.audience)(report);
    }

    /// Records where the extension stands.
    fn stand(&self, standing: Standing) {
        *self.standing.lock().unwrap_or_else(PoisonError::into_inner) = standing;
    }

    /// Says the extension is speaking.
    fn duty(&self) {
        self.stand(Standing::Duty);
    }

    /// Says why the extension stopped, once, to both the page and the standing.
    fn loss(&self, reason: String) {
        self.stand(Standing::Loss(reason.clone()));
        self.deliver(Report::Loss(reason));
    }

    /// Says the workspace has nothing to host.
    ///
    /// Reported through the same path as a loss so the page shows its banner
    /// over an empty surface, which is exactly what an absence looks like.
    fn vacancy(&self) {
        self.stand(Standing::Vacancy);
        self.deliver(Report::Loss(VACANCY.to_owned()));
    }

    /// Waits for a person to ask for a retry.
    ///
    /// Returns `None` when the host was stopped instead, which is the only
    /// other way this ends.
    fn retried(&self) -> Option<()> {
        while !self.halted() {
            match self.inbox.recv_timeout(POLL) {
                Ok(Order::Retry) => return Some(()),
                Ok(Order::Interaction(_) | Order::PaneProvider(_)) | Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    /// Waits out a restart delay, cut short by a retry or a shutdown.
    ///
    /// Returns whether the host should carry on.
    fn pause(&self, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        while Instant::now() < deadline {
            if self.halted() {
                return false;
            }
            if let Ok(Order::Retry) = self.inbox.try_recv() {
                return true;
            }
            std::thread::sleep(POLL);
        }
        !self.halted()
    }

    /// Drains the orders waiting for a running conversation.
    ///
    /// Returns why the session should end, when one of them says so.
    fn orders(&self, voice: &Voice) -> Option<Passage> {
        while let Ok(order) = self.inbox.try_recv() {
            match order {
                Order::Retry => return Some(Passage::Renewal),
                Order::Interaction(event) => speak(voice, &event),
                Order::PaneProvider(selection) => voice::speak_provider(voice, &selection),
            }
        }
        self.halted().then_some(Passage::Stopped)
    }
}

/// How one session ended.
enum Passage {
    /// The host was asked to stop.
    Stopped,
    /// A person asked for the extension to be started again.
    Renewal,
    /// The conversation, or the attempt to open one, ended. Carries what a
    /// person is shown.
    End(String),
}

/// The whole life of one host, on its own thread.
fn drive<S: Supply>(supply: &Arc<S>, hall: &Hall) {
    while !hall.halted() {
        let Some(plan) = prepare(supply, hall) else {
            return;
        };
        run(supply, hall, &plan);
    }
}

/// Reads the installation until there is something to host.
///
/// An absence is not a failure and not a hang: it is reported, and then this
/// waits for a retry in case the extension is installed while the page is open.
fn prepare<S: Supply>(supply: &Arc<S>, hall: &Hall) -> Option<Plan> {
    loop {
        match supply.plan() {
            Ok(Some(plan)) => return Some(plan),
            Ok(None) => hall.vacancy(),
            Err(reason) => hall.loss(reason),
        }
        hall.retried()?;
    }
}

/// Runs one plan for as long as its restart policy allows.
fn run<S: Supply>(supply: &Arc<S>, hall: &Hall, plan: &Plan) {
    let mut installation = Installation::new();
    if let Err(objection) = enrol(&mut installation, plan) {
        hall.loss(objection);
        return;
    }
    while !hall.halted() {
        match session(supply, hall, plan) {
            Passage::Stopped => break,
            Passage::Renewal => continue,
            Passage::End(reason) => hall.loss(reason),
        }
        if !recover(&mut installation, hall, plan) {
            break;
        }
    }
    supply.halt(plan);
}

/// Puts the record under the policy that decides its restarts.
///
/// The record is installed and enabled rather than trusted as it was read, so
/// the restart window is counted by [`Installation`] and not by this module.
fn enrol(installation: &mut Installation, plan: &Plan) -> Result<(), String> {
    installation
        .install(
            &plan.manifest,
            &plan.record.image_digest,
            &plan.record.granted,
            moment(),
        )
        .map_err(|objection| objection.to_string())?;
    installation
        .enable(&plan.record.name)
        .map(|_| ())
        .map_err(|objection| objection.to_string())
}

/// Asks the policy what to do with a sidecar that just stopped.
///
/// Returns whether the extension should be started again.
fn recover(installation: &mut Installation, hall: &Hall, plan: &Plan) -> bool {
    let Ok(disposition) = installation.restarted(&plan.record.name, moment()) else {
        return false;
    };
    match disposition {
        Disposition::Backoff { delay_ms, .. } => hall.pause(Duration::from_millis(delay_ms)),
        Disposition::Fault { restarts } => faulted(installation, hall, plan, restarts),
    }
}

/// Holds a crash-looping extension down until a person asks for it back.
fn faulted(installation: &mut Installation, hall: &Hall, plan: &Plan, restarts: u32) -> bool {
    hall.loss(format!(
        "{} stopped {restarts} times and will not be started again until you retry it",
        plan.record.name
    ));
    hall.deliver(Report::Fault { restarts });
    hall.retried().is_some() && installation.retry(&plan.record.name).is_ok()
}

/// One attempt: the container up, the socket open, and everything the
/// conversation collects carried to the page until it ends.
fn session<S: Supply>(supply: &Arc<S>, hall: &Hall, plan: &Plan) -> Passage {
    if let Err(reason) = supply.ensure(plan) {
        return Passage::End(reason);
    }
    let queue = Queue::new();
    let voice = Voice::default();
    let (finish, ended) = mpsc::sync_channel(1);
    let listener = match Listener::open(&plan.spec, attendant(supply, plan, &queue, &voice, finish)) {
        Ok(listener) => listener,
        Err(error) => return Passage::End(error.to_string()),
    };
    hall.duty();
    let passage = pump(hall, &queue, &voice, &ended);
    dismiss(listener);
    passage
}

/// Closes the socket and joins the thread serving it.
fn dismiss(listener: Listener) {
    let socket = listener.socket().to_path_buf();
    if let Err(fault) = listener.close() {
        hl_log::hl_error!(hl_log::tag::RUNTIME, "extension socket {}: {fault}", socket.display());
    }
}

/// Builds what the listener runs for each accepted connection.
fn attendant<S: Supply>(
    supply: &Arc<S>,
    plan: &Plan,
    queue: &Queue,
    voice: &Voice,
    finish: mpsc::SyncSender<String>,
) -> impl Fn(UnixStream) + Send + Sync + 'static {
    let supply = Arc::clone(supply);
    let queue = queue.clone();
    let voice = voice.clone();
    let held = Arc::new(plan.clone());
    move |stream| {
        let reason = converse(&supply, &held, &queue, &voice, stream);
        // A full channel means an earlier conversation's ending is still
        // waiting to be read, which is the one this would replace anyway.
        let _ = finish.try_send(reason);
    }
}

/// Serves one connection and reports how it ended.
fn converse<S: Supply>(supply: &Arc<S>, plan: &Plan, queue: &Queue, voice: &Voice, stream: UnixStream) -> String {
    let Ok(writer) = stream.try_clone() else {
        return "the extension's socket could not be duplicated".to_owned();
    };
    let opened = Conversation::new(stream, plan.authority(), plan.workspace.clone(), queue.clone());
    let Ok(mut conversation) = opened else {
        return "the extension's socket could not be duplicated".to_owned();
    };
    voice.hold(writer);
    let reason = exchange(supply, plan, &mut conversation);
    voice.release();
    reason
}

/// Greets the extension and serves it, reporting either ending as a sentence a
/// person can read.
fn exchange<S: Supply>(supply: &Arc<S>, plan: &Plan, conversation: &mut Conversation) -> String {
    if let Err(fault) = conversation.greet() {
        return fault.to_string();
    }
    match supply.attend(plan, conversation) {
        Ok(()) => format!("{} ended its session", plan.record.name),
        Err(reason) => reason,
    }
}

/// Carries interface work out and orders in until the session ends.
fn pump(hall: &Hall, queue: &Queue, voice: &Voice, ended: &mpsc::Receiver<String>) -> Passage {
    loop {
        collect(hall, queue);
        if let Some(passage) = hall.orders(voice) {
            return passage;
        }
        if let Ok(reason) = ended.try_recv() {
            // Once more, so work the extension left behind as it hung up is
            // shown rather than lost to the ending.
            collect(hall, queue);
            return Passage::End(reason);
        }
        std::thread::sleep(POLL);
    }
}

/// Moves everything the conversation collected to the page.
fn collect(hall: &Hall, queue: &Queue) {
    let interface = queue.collect();
    for frame in interface.frames {
        hall.deliver(Report::Frame(frame));
    }
    for mutation in interface.mutations {
        hall.deliver(Report::Source(mutation));
    }
}

/// Now, in milliseconds since the epoch, which is the clock
/// [`Installation`] is told about and does not own.
fn moment() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};

    use hl_extension::port::{
        ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
        TabSummary, TerminalSurface, WorkspaceFiles,
    };
    use hl_extension::{
        codec, Capability, ExtensionName, Grant, Hello, Installation, Manifest, Record, RelativePath, Request,
        Resources, Services, Transit, Wire, WorkspaceInfo, PROTOCOL,
    };

    use super::super::sidecar::Image;
    use super::{enrol, faulted, Hall, Host, Order, Plan, Report, SidecarSpec, Standing, Supply, VACANCY};
    use super::{Conversation, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// The source the fake extension's table draws from.
    const SOURCE: hl_gui::SourceId = hl_gui::SourceId::new(1);

    /// Waits for something another thread reaches on its own schedule.
    fn until(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// Everything the page was told, in order.
    #[derive(Clone, Default)]
    struct Gallery {
        held: Arc<Mutex<Vec<Report>>>,
    }

    impl Gallery {
        fn reports(&self) -> Vec<Report> {
            self.held.lock().expect("gallery").clone()
        }

        fn frames(&self) -> Vec<u64> {
            self.reports()
                .into_iter()
                .filter_map(|report| match report {
                    Report::Frame(frame) => Some(frame.sequence),
                    _ => None,
                })
                .collect()
        }

        fn losses(&self) -> Vec<String> {
            self.reports()
                .into_iter()
                .filter_map(|report| match report {
                    Report::Loss(reason) => Some(reason),
                    _ => None,
                })
                .collect()
        }

        fn faults(&self) -> Vec<u32> {
            self.reports()
                .into_iter()
                .filter_map(|report| match report {
                    Report::Fault { restarts } => Some(restarts),
                    _ => None,
                })
                .collect()
        }

        fn windows(&self) -> Vec<hl_gui::RowWindow> {
            self.reports()
                .into_iter()
                .filter_map(|report| match report {
                    Report::Source(hl_gui::SourceMutation::Window(window)) => Some(window),
                    _ => None,
                })
                .collect()
        }

        fn audience(&self) -> super::Audience {
            let held = Arc::clone(&self.held);
            Box::new(move |report| held.lock().expect("gallery").push(report))
        }
    }

    /// In-memory ports: no container runtime and no window.
    struct Ports;
    impl hl_extension::port::VolumeStore for Ports {}
    impl hl_extension::port::NetworkStore for Ports {}

    impl ContainerInventory for Ports {
        fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
            Ok(Vec::new())
        }

        fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
            Err(HostError::Absent(id.to_owned()))
        }
    }

    impl ContainerControl for Ports {
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

    impl ImageStore for Ports {
        fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
            Ok(Vec::new())
        }

        fn pull(&self, reference: &str) -> Result<ImageSummary, HostError> {
            Err(HostError::Absent(reference.to_owned()))
        }
    }

    impl TerminalSurface for Ports {
        fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
            Ok(Vec::new())
        }

        fn open_tab(&self, title: &str) -> Result<String, HostError> {
            Ok(title.to_owned())
        }

        fn split(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
            Ok("slot".to_owned())
        }

        fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
            Ok(())
        }

        fn read(&self, slot: &str, _lines: usize) -> Result<hl_extension::port::PaneText, HostError> {
            Ok(hl_extension::port::PaneText {
                slot: slot.to_owned(),
                lines: Vec::new(),
                truncated: false,
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
            Ok("slot".to_owned())
        }
    }

    impl hl_extension::port::WorkspaceInventory for Ports {
        fn workspaces(&self) -> Result<Vec<hl_extension::port::WorkspaceState>, HostError> {
            Ok(Vec::new())
        }
    }

    impl hl_extension::port::WorkspaceControl for Ports {}

    impl WorkspaceFiles for Ports {
        fn list(&self, _path: &RelativePath) -> Result<Vec<Entry>, HostError> {
            Ok(Vec::new())
        }

        fn read(&self, path: &RelativePath) -> Result<Vec<u8>, HostError> {
            Err(HostError::Absent(path.as_str().to_owned()))
        }

        fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
            Ok(())
        }
    }

    fn manifest() -> Manifest {
        Manifest {
            name: ExtensionName::new("sample").expect("name"),
            display_name: "Sample".to_owned(),
            version: "1.0.0".to_owned(),
            protocol: PROTOCOL,
            capabilities: Grant::new([Capability::ContainerRead, Capability::Interface]),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: Resources::default(),
            filesystem_roots: Vec::new(),
        }
    }

    fn plan(socket: &Path) -> Plan {
        let manifest = manifest();
        let record = Record {
            name: manifest.name.clone(),
            image_digest: "sha256:aaaa".to_owned(),
            version: "1.0.0".to_owned(),
            granted: manifest.capabilities.clone(),
            enabled: true,
            installed_at: 1,
            pane_providers: Vec::new(),
        };
        let spec = SidecarSpec::new(
            &manifest,
            &record.granted,
            &Image {
                reference: "extension:1".to_owned(),
                digest: "sha256:aaaa".to_owned(),
                entrypoint: vec!["/usr/bin/extension".to_owned()],
                user: "1000:1000".to_owned(),
            },
            socket,
        );
        Plan {
            record,
            manifest,
            spec,
            workspace: "dev".to_owned(),
        }
    }

    /// What a fake extension does once it is connected.
    #[derive(Clone, Copy)]
    struct Script {
        /// The sequence number of the frame it draws.
        sequence: u64,
        /// Whether it stays connected after drawing.
        linger: bool,
    }

    /// A supply with no container daemon: `ensure` starts a thread that
    /// connects to the host's own socket and speaks the protocol.
    struct Bench {
        socket: PathBuf,
        scripts: Mutex<Vec<Script>>,
        ensures: AtomicUsize,
        peers: Mutex<Vec<std::thread::JoinHandle<()>>>,
        token: Arc<()>,
    }

    impl Bench {
        /// A supply that runs each script in turn, one per attempt, and starts
        /// no peer once they run out.
        fn new(socket: &Path, scripts: &[Script], token: &Arc<()>) -> Self {
            Self {
                socket: socket.to_path_buf(),
                scripts: Mutex::new(scripts.iter().copied().rev().collect()),
                ensures: AtomicUsize::new(0),
                peers: Mutex::new(Vec::new()),
                token: Arc::clone(token),
            }
        }

        fn ensures(&self) -> usize {
            self.ensures.load(Ordering::Acquire)
        }
    }

    impl Supply for Bench {
        fn plan(&self) -> Result<Option<Plan>, String> {
            Ok(Some(plan(&self.socket)))
        }

        fn ensure(&self, _plan: &Plan) -> Result<(), String> {
            self.ensures.fetch_add(1, Ordering::Release);
            let Some(script) = self.scripts.lock().expect("scripts").pop() else {
                return Ok(());
            };
            let socket = self.socket.clone();
            let token = Arc::clone(&self.token);
            let peer = std::thread::spawn(move || {
                play(&socket, script);
                drop(token);
            });
            self.peers.lock().expect("peers").push(peer);
            Ok(())
        }

        fn attend(&self, _plan: &Plan, conversation: &mut Conversation) -> Result<(), String> {
            let ports = Ports;
            let services = Services {
                workspace: WorkspaceInfo {
                    name: "dev".to_owned(),
                    architecture: "arm64".to_owned(),
                    image: "alpine:3.20".to_owned(),
                },
                workspaces: &ports,
                workspace_control: &ports,
                containers: &ports,
                control: &ports,
                images: &ports,
                volumes: &ports,
                networks: &ports,
                terminal: &ports,
                files: &ports,
            };
            conversation.serve(&services).map_err(|fault| fault.to_string())
        }

        fn halt(&self, _plan: &Plan) {
            for peer in self.peers.lock().expect("peers").drain(..) {
                let _ = peer.join();
            }
        }
    }

    /// One fake extension: connect, handshake, draw, then answer row requests.
    fn play(socket: &Path, script: Script) {
        let Some(stream) = connect(socket) else {
            return;
        };
        let mut wire = Wire::new(stream);
        if shake(&mut wire).is_err() {
            return;
        }
        if describe(&mut wire, script.sequence).is_err() {
            return;
        }
        if script.linger {
            answer(&mut wire);
        }
    }

    /// Connects to a socket the host may not have bound yet.
    fn connect(socket: &Path) -> Option<UnixStream> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(stream) = UnixStream::connect(socket) {
                return Some(stream);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// Reads the welcome and answers it.
    fn shake(wire: &mut Wire<UnixStream>) -> Result<(), Transit> {
        let frame = wire.receive()?;
        codec::read_welcome(&frame).map_err(|coding| Transit::Io(coding.to_string()))?;
        let hello = Hello {
            protocol: PROTOCOL,
            name: ExtensionName::new("sample").expect("name"),
            features: Vec::new(),
        };
        wire.send(&codec::hello(&hello).expect("encoded"))
    }

    /// Opens a tab, draws one frame, and says how long its table is.
    fn describe(wire: &mut Wire<UnixStream>, sequence: u64) -> Result<(), Transit> {
        call(
            wire,
            &Request::InterfaceOpenTab {
                title: "Sample".to_owned(),
            },
        )?;
        call(
            wire,
            &Request::InterfaceRender {
                frame: hl_gui::Frame::new(sequence),
            },
        )
    }

    /// Answers row requests until the host closes the socket.
    fn answer(wire: &mut Wire<UnixStream>) {
        while let Ok(frame) = wire.receive() {
            let Ok(request) = serde_json::from_slice::<hl_gui::RowRequest>(&frame.payload) else {
                continue;
            };
            let window = hl_gui::SourceMutation::Window(hl_gui::RowWindow {
                source: request.source,
                version: request.version,
                request: request.id,
                range: request.range,
                rows: Vec::new(),
            });
            if call(wire, &Request::SourceResize { mutation: window }).is_err() {
                return;
            }
        }
    }

    /// Sends one call and reads its answer, so both ends stay in step.
    fn call(wire: &mut Wire<UnixStream>, request: &Request) -> Result<(), Transit> {
        wire.send(&codec::request(request).expect("encoded"))?;
        wire.receive().map(|_| ())
    }

    /// A row request as a table would report it.
    fn rows() -> hl_gui::Event {
        hl_gui::Event::Rows(hl_gui::RowRequest {
            id: hl_gui::RequestId::new(1),
            source: SOURCE,
            version: hl_gui::Version::new(1),
            range: hl_gui::RowRange::new(0, 8),
            sort: None,
            filter: None,
        })
    }

    /// A host over a temporary socket, with the reports it produced.
    fn hosted(socket: &Path, scripts: &[Script], token: &Arc<()>) -> (Host, Gallery) {
        let gallery = Gallery::default();
        let host = Host::open(Bench::new(socket, scripts, token), gallery.audience());
        (host, gallery)
    }

    #[test]
    fn a_frame_the_conversation_collects_reaches_the_page() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let token = Arc::new(());
        let (host, gallery) = hosted(
            &socket,
            &[Script {
                sequence: 1,
                linger: true,
            }],
            &token,
        );

        assert!(until(|| gallery.frames() == vec![1]), "the frame is posted");

        assert_eq!(host.standing(), Standing::Duty);
        host.close().expect("closed");
    }

    #[test]
    fn a_latched_crash_loop_reports_structured_state_before_retrying() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let plan = plan(&temporary.path().join("extension.sock"));
        let gallery = Gallery::default();
        let (orders, inbox) = mpsc::channel();
        orders.send(Order::Retry).expect("retry queued");
        let standing = Arc::new(Mutex::new(Standing::Duty));
        let hall = Hall {
            audience: gallery.audience(),
            inbox,
            standing: Arc::clone(&standing),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let mut installation = Installation::new();
        enrol(&mut installation, &plan).expect("enrolled");

        assert!(faulted(&mut installation, &hall, &plan, 5));

        assert_eq!(gallery.faults(), [5]);
        assert!(gallery.losses()[0].contains("stopped 5 times"));
        assert_eq!(installation.stage(&plan.record.name), hl_extension::Stage::Duty);
    }

    #[test]
    fn a_row_request_from_the_page_is_answered_by_the_extension() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let token = Arc::new(());
        let (host, gallery) = hosted(
            &socket,
            &[Script {
                sequence: 1,
                linger: true,
            }],
            &token,
        );
        assert!(until(|| !gallery.frames().is_empty()), "the extension is speaking");

        host.accept(Order::Interaction(rows()));

        assert!(until(|| !gallery.windows().is_empty()), "the window comes back");
        let window = gallery.windows().remove(0);
        assert_eq!(window.source, SOURCE);
        assert_eq!(window.request, hl_gui::RequestId::new(1));
        host.close().expect("closed");
    }

    #[test]
    fn a_conversation_that_ends_is_reported_once() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let token = Arc::new(());
        // One script: the restart that follows finds no peer, so a second loss
        // could only come from a second ending.
        let (host, gallery) = hosted(
            &socket,
            &[Script {
                sequence: 1,
                linger: false,
            }],
            &token,
        );

        assert!(until(|| gallery.losses().len() == 1), "the ending is reported");

        assert!(gallery.losses()[0].contains("sample"), "the reason names the extension");
        // Long enough for the restart the policy asks for, which finds no peer:
        // a second loss could only come from a second ending.
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(gallery.losses().len(), 1, "an ending is reported once");
        host.close().expect("closed");
    }

    #[test]
    fn a_retry_opens_the_conversation_again() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let token = Arc::new(());
        let gallery = Gallery::default();
        let bench = Arc::new(Bench::new(
            &socket,
            &[
                Script {
                    sequence: 1,
                    linger: true,
                },
                Script {
                    sequence: 2,
                    linger: true,
                },
            ],
            &token,
        ));
        let host = Host::open(Attendance(Arc::clone(&bench)), gallery.audience());
        assert!(until(|| gallery.frames() == vec![1]), "the first extension speaks");

        host.accept(Order::Retry);

        assert!(until(|| gallery.frames() == vec![1, 2]), "the second one speaks too");
        assert_eq!(bench.ensures(), 2, "the sequence was run again");
        host.close().expect("closed");
    }

    #[test]
    fn closing_the_host_ends_its_threads_and_leaves_no_socket() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        // Held by the peer thread, so the reference count is what outlived the
        // close.
        let token = Arc::new(());
        let (host, gallery) = hosted(
            &socket,
            &[Script {
                sequence: 1,
                linger: true,
            }],
            &token,
        );
        assert!(until(|| !gallery.frames().is_empty()), "a conversation is under way");

        host.close().expect("closed inside the deadline");

        assert_eq!(Arc::strong_count(&token), 1, "every thread the host started is joined");
        assert!(!socket.exists(), "the socket is not left behind");
        assert!(UnixStream::connect(&socket).is_err(), "the accept loop has stopped");
    }

    #[test]
    fn an_extension_that_is_not_installed_is_reported_as_an_absence() {
        let gallery = Gallery::default();

        let host = Host::open(Vacancy, gallery.audience());

        assert!(until(|| host.standing() == Standing::Vacancy), "the absence is stated");
        assert_eq!(gallery.losses(), vec![VACANCY.to_owned()], "the page is told why");
        assert!(gallery.frames().is_empty(), "nothing is drawn");
        host.close().expect("closed");
    }

    /// A workspace with nothing installed.
    struct Vacancy;

    impl Supply for Vacancy {
        fn plan(&self) -> Result<Option<Plan>, String> {
            Ok(None)
        }

        fn ensure(&self, _plan: &Plan) -> Result<(), String> {
            Ok(())
        }

        fn attend(&self, _plan: &Plan, _conversation: &mut Conversation) -> Result<(), String> {
            Ok(())
        }

        fn halt(&self, _plan: &Plan) {}
    }

    /// A supply the test keeps a handle on, so what the host asked for can be
    /// counted while the host owns the supply.
    struct Attendance(Arc<Bench>);

    impl Supply for Attendance {
        fn plan(&self) -> Result<Option<Plan>, String> {
            self.0.plan()
        }

        fn ensure(&self, plan: &Plan) -> Result<(), String> {
            self.0.ensure(plan)
        }

        fn attend(&self, plan: &Plan, conversation: &mut Conversation) -> Result<(), String> {
            self.0.attend(plan, conversation)
        }

        fn halt(&self, plan: &Plan) {
            self.0.halt(plan);
        }
    }
}
