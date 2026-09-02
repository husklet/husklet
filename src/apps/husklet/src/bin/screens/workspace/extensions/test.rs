//! What the extension shelf promises: a workspace's own extensions are on its
//! sidebar, our settings page drives the lifecycle policy, an image is only
//! recorded after somebody agreed to what it asks for, and a click on a
//! rendered widget reaches the extension that drew it.

use std::cell::RefCell;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use hl::extension::{Acquisition, Candidate, Roster};
use hl_extension::{Capability, ExtensionName, Grant, Manifest, Record, Stage, Wire, PROTOCOL};
use hl_ws::storage::Directory;

use super::super::{Page, View};
use super::{directory, settings, Catalogue, Inspection, Shared, Shelf, Surfaces};

/// The style class the fake surface carries, so a test can tell an extension's
/// own page from the settings page beside it.
const SURFACE: &str = "hl-test-surface";

/// Every scenario runs inside one test, on the binary's toolkit thread.
///
/// GTK belongs to whichever thread entered it and libtest gives every `#[test]`
/// its own, so the scenarios are handed to `test_support`, which owns the one
/// thread in this process that entered GTK. Entering it here instead is what
/// used to make this test either SIGSEGV on a display-less host or panic beside
/// the extension-page test on a host with a display; `test_support` documents
/// both mechanisms.
#[test]
fn a_workspaces_extensions_are_on_its_sidebar_and_hear_what_is_clicked() {
    let ran = crate::test_support::on_the_toolkit_thread(|| {
        the_sidebar_lists_exactly_what_the_workspace_recorded();
        selecting_an_extension_shows_the_surface_it_draws();
        the_settings_page_says_where_an_extension_stands();
        the_settings_actions_drive_the_installation();
        removing_an_extension_takes_its_pages_with_it();
        an_image_is_read_before_anybody_is_asked();
        remote_image_progress_precedes_the_consent_prompt();
        a_declined_image_records_nothing();
        a_click_on_a_rendered_button_reaches_the_extension();
        panes::reading_a_pane_hands_back_what_was_written_to_it();
        panes::a_pane_read_never_answers_with_more_than_it_was_allowed();
        panes::dividing_a_pane_produces_a_slot_that_can_be_addressed();
        panes::closing_a_pane_by_slot_removes_that_one_and_leaves_the_rest();
        panes::a_pane_can_hold_an_extensions_interface_beside_a_shell();
        panes::a_pane_chooser_switches_to_a_provider_and_back_to_its_shell();
        panes::an_existing_pane_chooser_discovers_a_later_provider();
        panes::every_split_leaf_owns_its_chooser_and_topology_is_nested();
        panes::splitting_an_interface_again_moves_its_one_surface();
        panes::a_failed_interface_split_leaves_its_surface_where_it_was();
        panes::a_restored_surface_without_its_extension_is_frozen_rather_than_a_shell();
    });
    if !ran {
        eprintln!("skipped: no display connection, so the extension shelf cannot be rendered");
    }
}

/// One shell, one roster, and the shelf between them.
struct Fixture {
    _storage: tempfile::TempDir,
    view: Rc<View>,
    roster: Shared,
    shelf: Rc<Shelf>,
}

impl Fixture {
    /// A shelf over a roster holding `recorded`, on a shell with the fixed pages.
    fn new(recorded: &[(&str, bool)]) -> Self {
        let storage = tempfile::tempdir().expect("temporary directory");
        let roster = Rc::new(RefCell::new(
            Roster::open(Directory::open(storage.path()).expect("storage")).expect("roster"),
        ));
        for (name, enabled) in recorded {
            record(&roster, name, *enabled);
        }
        let view = Rc::new(View::new([
            (Page::Overview, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
            (Page::Extensions, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
        ]));
        let surfaces: Surfaces = Rc::new(|_| {
            let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
            widget.add_css_class(SURFACE);
            widget.upcast()
        });
        let shelf = Shelf::new(&view, &roster, surfaces);
        shelf.install();
        Self {
            _storage: storage,
            view,
            roster,
            shelf,
        }
    }

    /// Where one extension stands, as the roster now says.
    fn stage(&self, name: &str) -> Stage {
        self.roster.borrow().stage(&named(name))
    }

    /// The first widget on a page carrying a style class.
    fn tagged(&self, page: &str, class: &str) -> Option<gtk::Widget> {
        let page = self.view.page(page)?;
        descendants(&page)
            .into_iter()
            .find(|widget| widget.has_css_class(class))
    }

    /// Clicks the action on a settings page carrying a style class.
    fn act(&self, name: &str, class: &str) {
        self.tagged(&super::settings_title(&named(name)), class)
            .unwrap_or_else(|| panic!("{class} is offered on {name}'s settings page"))
            .downcast::<gtk::Button>()
            .expect("an action is a button")
            .emit_clicked();
    }
}

/// Writes one record straight through the roster, which is what an install did.
fn record(roster: &Shared, name: &str, enabled: bool) {
    let manifest = manifest(name);
    let mut held = roster.borrow_mut();
    held.register(&manifest, "sha256:aaaa", &manifest.capabilities, 1)
        .expect("registered");
    if enabled {
        held.enable(&manifest.name).expect("enabled");
    }
}

fn named(name: &str) -> ExtensionName {
    ExtensionName::new(name).expect("name")
}

fn manifest(name: &str) -> Manifest {
    Manifest {
        name: named(name),
        display_name: name.to_owned(),
        version: "1.0.0".to_owned(),
        protocol: PROTOCOL,
        capabilities: Grant::new([Capability::Interface, Capability::ContainerRead]),
        entrypoint: None,
        activation: hl_extension::Activation::default(),
        interface: None,
        pane_providers: Vec::new(),
        resources: hl_extension::Resources::default(),
        filesystem_roots: Vec::new(),
    }
}

/// Every widget under one, parents before children.
fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![widget.clone()];
    let mut index = 0;
    while index < found.len() {
        let mut cursor = found[index].first_child();
        while let Some(child) = cursor {
            cursor = child.next_sibling();
            found.push(child);
        }
        index += 1;
    }
    found
}

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

fn the_sidebar_lists_exactly_what_the_workspace_recorded() {
    let fixture = Fixture::new(&[("alpha", false), ("zulu", true)]);

    let listed = fixture.view.entries();

    assert!(listed.contains(&"alpha".to_owned()), "got {listed:?}");
    assert!(listed.contains(&"alpha settings".to_owned()), "got {listed:?}");
    assert!(listed.contains(&"zulu".to_owned()), "got {listed:?}");
    assert!(
        !listed.contains(&"other".to_owned()),
        "only this workspace's extensions are listed"
    );
    assert_eq!(
        listed.iter().filter(|entry| entry.as_str() == "alpha").count(),
        1,
        "one entry per extension"
    );
}

fn selecting_an_extension_shows_the_surface_it_draws() {
    let fixture = Fixture::new(&[("alpha", true)]);

    fixture.view.select_name("alpha");

    assert_eq!(fixture.view.shown().as_deref(), Some("alpha"));
    assert!(
        fixture.tagged("alpha", SURFACE).is_some(),
        "the extension's own surface is the page"
    );
    assert!(
        fixture.tagged("alpha settings", SURFACE).is_none(),
        "our settings page is not the extension's surface"
    );
}

fn the_settings_page_says_where_an_extension_stands() {
    let fixture = Fixture::new(&[("alpha", true), ("zulu", false)]);

    let duty = fixture
        .tagged("alpha settings", settings::STANDING)
        .and_downcast::<gtk::Label>()
        .expect("a standing");
    let standby = fixture
        .tagged("zulu settings", settings::STANDING)
        .and_downcast::<gtk::Label>()
        .expect("a standing");

    assert_eq!(duty.text(), "enabled");
    assert_eq!(standby.text(), "disabled");
    assert!(
        fixture.tagged("alpha settings", settings::DISABLE).is_some(),
        "an enabled extension is offered a disable"
    );
    assert!(
        fixture.tagged("zulu settings", settings::ENABLE).is_some(),
        "a disabled extension is offered an enable"
    );
}

fn the_settings_actions_drive_the_installation() {
    let fixture = Fixture::new(&[("alpha", false)]);
    assert_eq!(fixture.stage("alpha"), Stage::Standby);

    fixture.act("alpha", settings::ENABLE);
    assert_eq!(fixture.stage("alpha"), Stage::Duty, "the policy was told");

    fixture.act("alpha", settings::DISABLE);
    assert_eq!(fixture.stage("alpha"), Stage::Standby, "and told again");
    assert!(
        fixture.tagged("alpha settings", settings::ENABLE).is_some(),
        "the page was rebuilt from what the policy now says"
    );
}

fn removing_an_extension_takes_its_pages_with_it() {
    let fixture = Fixture::new(&[("alpha", true)]);

    fixture.act("alpha", settings::REMOVE);

    assert_eq!(fixture.stage("alpha"), Stage::Vacancy, "the record is forgotten");
    assert!(!fixture.view.holds("alpha"), "its surface is off the shell");
    assert!(!fixture.view.holds("alpha settings"), "and so is its settings page");
    assert!(
        !fixture.view.entries().contains(&"alpha".to_owned()),
        "and its sidebar entry is gone"
    );
}

/// A catalogue whose inspection answers with `answer`, with nothing installed.
fn catalogue(fixture: &Fixture, answer: Result<Candidate, String>) -> Rc<Catalogue> {
    let held = Mutex::new(Some(answer));
    let inspection: Inspection = Rc::new(move |_| {
        let (answered, answer) = std::sync::mpsc::channel();
        let taken = held.lock().expect("answer").take();
        if let Some(taken) = taken {
            let event = match taken {
                Ok(candidate) => Acquisition::Ready(candidate),
                Err(reason) => Acquisition::Failed(reason),
            };
            let _ = answered.send(event);
        }
        answer
    });
    Catalogue::new(&fixture.shelf, inspection)
}

/// Types an image reference into the page's own field, which is where the
/// page reads it from.
fn typed(page: &Rc<Catalogue>, reference: &str) {
    descendants(page.widget().clone().upcast_ref())
        .into_iter()
        .find(|widget| widget.has_css_class(directory::REFERENCE))
        .and_downcast::<gtk::Entry>()
        .expect("a field to type the image into")
        .set_text(reference);
}

fn candidate() -> Candidate {
    Candidate {
        reference: "sample:1".to_owned(),
        digest: "sha256:bbbb".to_owned(),
        manifest: manifest("sample"),
    }
}

fn an_image_is_read_before_anybody_is_asked() {
    let fixture = Fixture::new(&[]);
    let page = catalogue(&fixture, Ok(candidate()));
    typed(&page, "sample:1");

    page.inspect();
    assert!(page.poll(), "the inspection came back");

    assert!(
        fixture.roster.borrow().entries().is_empty(),
        "reading an image records nothing on its own"
    );
    assert!(
        page.notice().contains("asks for"),
        "what it asks for is put to a person, got {:?}",
        page.notice()
    );

    page.consent();

    let entries = fixture.roster.borrow().entries();
    assert_eq!(entries.len(), 1, "consent is what records the grant");
    assert_eq!(entries[0].image_digest, "sha256:bbbb");
    assert!(entries[0].granted.holds(Capability::Interface));
    assert_eq!(entries[0].stage, Stage::Standby, "an install starts off duty");
    assert!(
        fixture.view.holds("sample"),
        "and it is on the sidebar without a restart"
    );
    assert!(fixture.view.holds("sample settings"));
}

fn a_declined_image_records_nothing() {
    let fixture = Fixture::new(&[]);
    let page = catalogue(&fixture, Ok(candidate()));
    typed(&page, "sample:1");
    page.inspect();
    assert!(page.poll(), "the inspection came back");

    page.decline();

    assert!(fixture.roster.borrow().entries().is_empty(), "nothing was recorded");
    assert!(!fixture.view.holds("sample"), "and nothing reached the sidebar");
    page.consent();
    assert!(
        fixture.roster.borrow().entries().is_empty(),
        "a declined candidate cannot be installed afterwards"
    );
}

fn remote_image_progress_precedes_the_consent_prompt() {
    let fixture = Fixture::new(&[]);
    let events = Mutex::new(Some(vec![
        Acquisition::Inspecting,
        Acquisition::Pulling {
            status: "Pulling from team/tool".to_owned(),
            id: Some("team/tool:latest".to_owned()),
            current: None,
            total: None,
        },
        Acquisition::ReadingManifest,
        Acquisition::Ready(candidate()),
    ]));
    let inspection: Inspection = Rc::new(move |_| {
        let (sent, received) = std::sync::mpsc::channel();
        if let Some(events) = events.lock().expect("events").take() {
            for event in events {
                sent.send(event).expect("catalogue is listening");
            }
        }
        received
    });
    let page = Catalogue::new(&fixture.shelf, inspection);
    typed(&page, "team/tool:latest");
    page.inspect();

    assert!(page.poll());
    assert_eq!(page.notice(), "checking local images");
    assert!(page.poll());
    assert!(page.notice().contains("Pulling from team/tool"));
    assert!(fixture.roster.borrow().entries().is_empty(), "progress is not consent");
    assert!(page.poll());
    assert_eq!(page.notice(), "reading extension manifest");
    assert!(page.poll());
    assert!(page.notice().contains("asks for"));
    assert!(
        fixture.roster.borrow().entries().is_empty(),
        "a ready image still awaits consent"
    );
}

/// What the fake extension heard, in order.
type Heard = Arc<Mutex<Vec<String>>>;

/// A supply with no container daemon: `ensure` starts a thread that connects to
/// the host's own socket, speaks the handshake, and then listens.
struct Bench {
    socket: std::path::PathBuf,
    heard: Heard,
    greeted: Arc<AtomicBool>,
    peers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl hl::extension::Supply for Bench {
    fn plan(&self) -> Result<Option<hl::extension::Plan>, String> {
        let manifest = manifest("sample");
        let record = Record {
            name: manifest.name.clone(),
            image_digest: "sha256:aaaa".to_owned(),
            granted: manifest.capabilities.clone(),
            enabled: true,
            installed_at: 1,
            pane_providers: manifest.pane_providers.clone(),
        };
        let image = hl::extension::Image {
            reference: "extension:1".to_owned(),
            digest: "sha256:aaaa".to_owned(),
            entrypoint: vec!["/usr/bin/extension".to_owned()],
            user: "1000:1000".to_owned(),
        };
        let spec = hl::extension::SidecarSpec::new(&manifest, &record.granted, &image, &self.socket);
        Ok(Some(hl::extension::Plan {
            record,
            manifest,
            spec,
            workspace: "dev".to_owned(),
        }))
    }

    fn ensure(&self, _plan: &hl::extension::Plan) -> Result<(), String> {
        let socket = self.socket.clone();
        let heard = Arc::clone(&self.heard);
        let greeted = Arc::clone(&self.greeted);
        self.peers
            .lock()
            .expect("peers")
            .push(std::thread::spawn(move || listen(&socket, &heard, &greeted)));
        Ok(())
    }

    fn attend(
        &self,
        _plan: &hl::extension::Plan,
        conversation: &mut hl::extension::Conversation,
    ) -> Result<(), String> {
        // The extension in this suite only listens, so the session ends when it
        // hangs up. Reading is what notices that.
        conversation
            .serve(&ports::services())
            .map_err(|fault| fault.to_string())
    }

    fn halt(&self, _plan: &hl::extension::Plan) {
        for peer in self.peers.lock().expect("peers").drain(..) {
            let _ = peer.join();
        }
    }
}

/// The fake extension: connect, handshake, then write down every interaction
/// the host sends.
fn listen(socket: &Path, heard: &Heard, greeted: &AtomicBool) {
    let Some(stream) = connect(socket) else {
        return;
    };
    let mut wire = Wire::new(stream);
    if shake(&mut wire).is_err() {
        return;
    }
    greeted.store(true, Ordering::Release);
    while let Ok(frame) = wire.receive() {
        let Ok(said) = serde_json::from_slice::<serde_json::Value>(&frame.payload) else {
            continue;
        };
        let Some(id) = said.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        heard.lock().expect("heard").push(id.to_owned());
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
fn shake(wire: &mut Wire<UnixStream>) -> Result<(), hl_extension::Transit> {
    let frame = wire.receive()?;
    hl_extension::codec::read_welcome(&frame).map_err(|coding| hl_extension::Transit::Io(coding.to_string()))?;
    let hello = hl_extension::Hello {
        protocol: PROTOCOL,
        name: named("sample"),
        features: Vec::new(),
    };
    wire.send(&hl_extension::codec::hello(&hello).expect("encoded"))
}

fn a_click_on_a_rendered_button_reaches_the_extension() {
    use super::super::extension::{channel, Delivery, Interface, Signal};
    use hl_gui::{Element, EventId, Reconciliation};

    let temporary = tempfile::tempdir().expect("temporary directory");
    let socket = temporary.path().join("run/extension.sock");
    let heard: Heard = Arc::default();
    let greeted = Arc::new(AtomicBool::new(false));
    let host = Rc::new(hl::extension::Host::open(
        Bench {
            socket,
            heard: Arc::clone(&heard),
            greeted: Arc::clone(&greeted),
            peers: Mutex::new(Vec::new()),
        },
        Box::new(|_| ()),
    ));
    assert!(
        until(|| host.standing() == hl::extension::Standing::Duty),
        "the extension connected, got {:?}",
        host.standing()
    );
    // `Duty` is set when the socket is bound, which is earlier than a
    // connection: an interaction handed over before one is accepted is written
    // to nobody and dropped, because the host's writing end is still empty. The
    // host takes that end before it greets, so an extension that has read the
    // welcome is one the host can already speak to -- which is what a click has
    // to wait for, rather than for the standing.
    assert!(
        until(|| greeted.load(Ordering::Acquire)),
        "the extension read the host's welcome"
    );

    let (post, deliveries) = channel();
    let orders = Rc::clone(&host);
    let (widget, mut page) = Interface::new(
        deliveries,
        Rc::new(move |signal: Signal| match signal {
            Signal::Interaction(event) => orders.accept(hl::extension::Order::Interaction(event)),
            Signal::Retry => orders.accept(hl::extension::Order::Retry),
        }),
    );
    let described = Element::column().child(Element::button("Restart", EventId::new("restart")).key("restart"));
    let frame = Reconciliation::new().reconcile(&described);
    post.send(Delivery::Frame(frame)).expect("the page is listening");
    page.tick();

    let button = descendants(&widget.clone().upcast())
        .into_iter()
        .find(|found| found.has_css_class("hl-button"))
        .expect("the button reached the page")
        .downcast::<gtk::Button>()
        .expect("a button tag builds a button");
    button.emit_clicked();
    page.tick();

    assert!(
        until(|| heard.lock().expect("heard").iter().any(|id| id == "restart")),
        "the click reached the extension, it heard {:?}",
        heard.lock().expect("heard")
    );
    drop(host);
}

/// In-memory ports, so a conversation can be served with no container runtime
/// and no window.
mod ports {
    use hl_extension::port::{
        ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
        PaneText, TabSummary, TerminalSurface, WorkspaceFiles, WorkspaceInventory, WorkspaceState,
    };
    use hl_extension::{RelativePath, Services, WorkspaceInfo};

    /// The one value every port of this fake host is served from.
    pub(super) struct Ports;

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
            Ok(name.to_owned())
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

        fn split(&self, slot: &str, _division: Division) -> Result<String, HostError> {
            Ok(slot.to_owned())
        }

        fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
            Ok(())
        }

        fn read(&self, slot: &str, _lines: usize) -> Result<PaneText, HostError> {
            Ok(PaneText {
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

        fn surface(&self, slot: &str, _division: Division) -> Result<String, HostError> {
            Ok(slot.to_owned())
        }
    }

    impl WorkspaceInventory for Ports {
        fn workspaces(&self) -> Result<Vec<WorkspaceState>, HostError> {
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

    /// The services one fake conversation is served against.
    pub(super) fn services() -> Services<'static> {
        static PORTS: Ports = Ports;
        Services {
            workspace: WorkspaceInfo {
                name: "dev".to_owned(),
                architecture: "arm64".to_owned(),
                image: "alpine:3.20".to_owned(),
            },
            workspaces: &PORTS,
            workspace_control: &PORTS,
            containers: &PORTS,
            control: &PORTS,
            images: &PORTS,
            terminal: &PORTS,
            files: &PORTS,
        }
    }
}

/// What a socket can do to the panes of a window: read one, restructure the
/// tab, and put an extension's own interface in a pane beside a shell.
///
/// These run against a window built without an application behind it, because
/// every one of them is about the widget tree and the pane registries rather
/// than about a presented window or a running workspace.
mod panes {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use hl_extension::port::{Division, HostError, LayoutNode, Occupant};
    use hl_extension::ExtensionName;
    use hl_ws_term::session::{PaneNode, SurfacePane};

    use super::super::super::terminal::{
        Adjustment, PaneChooser, PaneChrome, Panes, ProductionPaneLauncher, Reading, Slots, Surface, Tabs, TermWin,
        Window, WindowSession, ABSENCE,
    };
    use super::super::Console;
    use super::super::Gallery;
    use hl::config::WorkspaceConfig;
    use vte4::prelude::TerminalExt as _;

    /// A window with one tab holding one terminal pane.
    struct Bench {
        window: Rc<TermWin>,
        page: gtk::Box,
    }

    impl Bench {
        fn new() -> Self {
            let workspace = WorkspaceConfig::new("dev", "alpine:3.20", hl_ws::Arch::Arm64);
            let window = Window::bench(&workspace);
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            page.set_hexpand(true);
            page.set_vexpand(true);
            drop(Tabs::new(&window).add("shell 1", None, &page, true));
            Self { window, page }
        }

        /// A terminal pane in the tab, registered under a fresh slot.
        fn shell(&self) -> (vte4::Terminal, String) {
            let terminal = vte4::Terminal::new();
            let slot = Window::slot(&self.window);
            Slots::new(&self.window).hold(&terminal, slot.clone());
            self.page.append(&PaneChrome::wrap(&self.window, &terminal));
            (terminal, slot)
        }

        /// Another terminal pane, beside an existing one.
        fn beside(&self, pane: &vte4::Terminal) -> (vte4::Terminal, String) {
            let terminal = vte4::Terminal::new();
            let slot = Window::slot(&self.window);
            Slots::new(&self.window).hold(&terminal, slot.clone());
            assert!(
                Panes::divide(
                    &self.window,
                    &Slots::new(&self.window).of(pane).expect("slot"),
                    gtk::Orientation::Horizontal,
                    terminal.upcast_ref()
                ),
                "a pane in a tab can be divided"
            );
            (terminal, slot)
        }

        /// Every slot the window currently holds.
        fn slots(&self) -> Vec<String> {
            Panes::all(&self.window).into_iter().map(|pane| pane.slot).collect()
        }
    }

    /// Runs the main loop until a condition holds, which is how text fed to a
    /// terminal becomes text the terminal is showing.
    fn until(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        condition()
    }

    /// What a bounded read of one pane answered with.
    fn lines(bench: &Bench, slot: &str, bound: usize) -> Vec<String> {
        match Panes::read(&bench.window, slot, bound) {
            Reading::Text(text) => text.lines,
            other => panic!("a shell pane shows text, got {other:?}"),
        }
    }

    pub(super) fn reading_a_pane_hands_back_what_was_written_to_it() {
        let bench = Bench::new();
        let (terminal, slot) = bench.shell();
        terminal.feed(b"the quick brown fox\r\n");

        assert!(
            until(|| lines(&bench, &slot, 100)
                .iter()
                .any(|line| line.contains("quick brown"))),
            "the pane hands back what was written to it, got {:?}",
            lines(&bench, &slot, 100)
        );
        assert_eq!(
            Panes::read(&bench.window, "no-such-pane", 100),
            Reading::Absent,
            "a slot naming no pane is refused rather than answered with nothing"
        );
    }

    pub(super) fn a_pane_read_never_answers_with_more_than_it_was_allowed() {
        let bench = Bench::new();
        let (terminal, slot) = bench.shell();
        for index in 0..60 {
            terminal.feed(format!("line {index}\r\n").as_bytes());
        }
        assert!(
            until(|| lines(&bench, &slot, 200).iter().any(|line| line.contains("line 59"))),
            "the pane caught up with what was fed to it"
        );

        let Reading::Text(bounded) = Panes::read(&bench.window, &slot, 5) else {
            panic!("a shell pane shows text");
        };

        assert!(
            bounded.lines.len() <= 5,
            "the bound bounds, got {}",
            bounded.lines.len()
        );
        assert!(bounded.truncated, "and says that older lines were left behind");
        assert!(
            bounded.lines.last().is_some_and(|line| line.contains("line 59")),
            "the tail is what is kept, got {:?}",
            bounded.lines
        );
    }

    pub(super) fn dividing_a_pane_produces_a_slot_that_can_be_addressed() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let before = bench.slots();

        let (_second, two) = bench.beside(&first);

        assert!(!before.contains(&two), "the slot is new");
        assert_eq!(bench.slots().len(), before.len() + 1, "and there is one more pane");
        assert!(Panes::at(&bench.window, &two).is_some(), "addressable by its own slot");
        assert_eq!(Panes::ratio(&bench.window, &two, 0.25), Adjustment::Set);
        assert_eq!(
            Panes::ratio(&bench.window, "no-such-pane", 0.25),
            Adjustment::Absent,
            "a ratio for a pane that is not there is refused"
        );
        assert!(Panes::focus(&bench.window, &one), "focus moves by slot");
    }

    pub(super) fn closing_a_pane_by_slot_removes_that_one_and_leaves_the_rest() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let (_second, two) = bench.beside(&first);

        assert!(Panes::close(&bench.window, &two), "the pane was closed");

        assert_eq!(bench.slots(), vec![one], "exactly the named pane is gone");
        assert!(!Panes::close(&bench.window, &two), "and closing it again finds nothing");
    }

    pub(super) fn a_pane_can_hold_an_extensions_interface_beside_a_shell() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        interface.add_css_class(super::SURFACE);
        home.append(&interface);
        gallery.enrol("sample", &interface, &home, &[], Rc::new(|_| {}));
        Window::exhibit(&bench.window, gallery.clone());
        let slot = Window::slot(&bench.window);
        let pane = Surface::build(&bench.window, "sample", None, slot.clone());
        assert!(Panes::divide(&bench.window, &one, gtk::Orientation::Horizontal, &pane));

        let held = Panes::at(&bench.window, &slot).expect("the surface pane is addressable");
        assert_eq!(held.occupant, Occupant::Surface, "and says what is in it");
        assert!(
            super::descendants(&pane)
                .iter()
                .any(|found| found == interface.upcast_ref::<gtk::Widget>()),
            "the extension's own interface is what the pane holds"
        );
        assert!(
            Panes::all(&bench.window).iter().any(|pane| pane.slot == one),
            "the shell beside it is still a pane"
        );
        assert!(
            matches!(Panes::read(&bench.window, &slot, 10), Reading::Drawn),
            "and it is not pretending to be a shell"
        );

        assert!(
            Panes::close(&bench.window, &slot),
            "the surface pane closes like any other"
        );
        assert_eq!(
            interface.parent().as_ref(),
            Some(home.upcast_ref::<gtk::Widget>()),
            "and hands the interface back to its page rather than taking it away"
        );
        drop(first);
    }

    pub(super) fn a_pane_chooser_switches_to_a_provider_and_back_to_its_shell() {
        let bench = Bench::new();
        let (terminal, slot) = bench.shell();
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        let selected = Rc::new(RefCell::new(None));
        let selection = Rc::clone(&selected);
        gallery.enrol(
            "postgres",
            &interface,
            &home,
            &[hl_extension::PaneProvider {
                id: ExtensionName::new("database").expect("provider id"),
                title: "Postgres".to_owned(),
                icon: None,
            }],
            Rc::new(move |provider| *selection.borrow_mut() = Some(provider)),
        );
        Window::exhibit(&bench.window, gallery.clone());
        let chrome = Panes::at(&bench.window, &slot).expect("pane chrome").widget;

        assert_eq!(gallery.providers()[0].title, "Postgres");
        PaneChooser::provider(&bench.window, "postgres", "database");
        assert_eq!(
            Panes::at(&bench.window, &slot).expect("switched pane").occupant,
            Occupant::Surface
        );
        assert!(
            gallery.holds("postgres"),
            "the overview remains registered while its interface is borrowed"
        );
        assert_eq!(
            selected
                .borrow()
                .as_ref()
                .map(|selection| selection.pane_provider.as_str()),
            Some("database"),
            "the extension is told which named view it should render"
        );
        let topology = Console::topology(&bench.window).expect("provider topology");
        let LayoutNode::Pane { pane, .. } = &topology.tabs[0].root else {
            panic!("the unsplit provider is one pane")
        };
        let identity = pane.provider.as_ref().expect("surface provider identity");
        assert_eq!(identity.extension, "postgres");
        assert_eq!(identity.provider, "database");

        PaneChooser::terminal(&bench.window);
        let restored = Panes::at(&bench.window, &slot).expect("restored pane");
        assert_eq!(restored.occupant, Occupant::Terminal);
        assert_eq!(
            restored.widget, chrome,
            "the pane keeps one stable chrome across occupants"
        );
        assert_eq!(restored.content, terminal.upcast::<gtk::Widget>());
        assert_eq!(interface.parent().as_ref(), Some(home.upcast_ref::<gtk::Widget>()));
    }

    pub(super) fn an_existing_pane_chooser_discovers_a_later_provider() {
        let bench = Bench::new();
        let chooser = PaneChooser::button(&bench.window);
        let labels = || {
            chooser
                .popover()
                .into_iter()
                .flat_map(|popover| super::descendants(popover.upcast_ref::<gtk::Widget>()))
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .filter_map(|button| button.label())
                .map(|label| label.to_string())
                .collect::<Vec<String>>()
        };
        assert_eq!(labels(), ["Terminal"], "the chooser exists before providers do");

        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol(
            "postgres",
            &interface,
            &home,
            &[hl_extension::PaneProvider {
                id: ExtensionName::new("database").expect("provider id"),
                title: "Postgres".to_owned(),
                icon: None,
            }],
            Rc::new(|_| {}),
        );
        Window::exhibit(&bench.window, gallery);
        PaneChooser::populate(&bench.window, &chooser);
        assert_eq!(
            labels(),
            ["Terminal", "Postgres"],
            "an old tab reads the live catalogue"
        );
    }

    pub(super) fn every_split_leaf_owns_its_chooser_and_topology_is_nested() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let (_second, two) = bench.beside(&first);
        for slot in [&one, &two] {
            let pane = Panes::at(&bench.window, slot).expect("split leaf");
            assert!(PaneChrome::is(&pane.widget), "{slot} has stable pane chrome");
            assert!(
                super::descendants(&pane.widget)
                    .iter()
                    .any(|widget| widget.is::<gtk::MenuButton>()),
                "{slot} owns its chooser"
            );
        }

        let topology = Console::topology(&bench.window).expect("topology");
        assert_eq!(topology.active_tab.as_deref(), Some("p0"));
        assert_eq!(topology.tabs.len(), 1);
        let LayoutNode::Split {
            division,
            first,
            second,
            ..
        } = &topology.tabs[0].root
        else {
            panic!("two leaves are reported as one nested split")
        };
        assert_eq!(*division, Division::Beside);
        let slots = [first.as_ref(), second.as_ref()].map(|node| match node {
            LayoutNode::Pane { pane, .. } => pane.slot.as_str(),
            LayoutNode::Split { .. } => panic!("a leaf became another split"),
        });
        assert_eq!(slots, [one.as_str(), two.as_str()]);
    }

    pub(super) fn splitting_an_interface_again_moves_its_one_surface() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let (_second, two) = bench.beside(&first);
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        interface.add_css_class(super::SURFACE);
        home.append(&interface);
        gallery.enrol("sample", &interface, &home, &[], Rc::new(|_| {}));
        Window::exhibit(&bench.window, gallery);

        let old = Console::surface(&bench.window, Some("sample"), &one, Division::Below).expect("the first surface");
        let moved =
            Console::surface(&bench.window, Some("sample"), &two, Division::Below).expect("the relocated surface");

        assert_ne!(moved, old, "the new pane has its own authoritative slot");
        assert!(Panes::at(&bench.window, &old).is_none(), "the old holder was collapsed");
        let held = Panes::at(&bench.window, &moved).expect("the returned slot names the new pane");
        assert_eq!(held.occupant, Occupant::Surface);
        assert!(
            super::descendants(&held.widget)
                .iter()
                .any(|found| found == interface.upcast_ref::<gtk::Widget>()),
            "the same interface widget moved rather than a second tree being built"
        );
        assert_eq!(
            super::descendants(bench.page.upcast_ref::<gtk::Widget>())
                .iter()
                .filter(|found| *found == interface.upcast_ref::<gtk::Widget>())
                .count(),
            1,
            "the interface appears exactly once in the layout"
        );
    }

    pub(super) fn a_failed_interface_split_leaves_its_surface_where_it_was() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol("sample", &interface, &home, &[], Rc::new(|_| {}));
        Window::exhibit(&bench.window, gallery);
        let old = Console::surface(&bench.window, Some("sample"), &one, Division::Beside).expect("the first surface");
        let before = interface.parent();
        // A registered pane under a Grid is addressable, but PaneSplit cannot
        // restructure that parent: terminal layouts accept only their Box and
        // Paned shapes. This reaches the post-borrow rollback path rather than
        // the missing-slot preflight.
        let unsupported = vte4::Terminal::new();
        let unsupported_slot = Window::slot(&bench.window);
        Slots::new(&bench.window).hold(&unsupported, unsupported_slot.clone());
        let grid = gtk::Grid::new();
        grid.attach(&PaneChrome::wrap(&bench.window, &unsupported), 0, 0, 1, 1);
        bench.page.append(&grid);

        let failure = Console::surface(&bench.window, Some("sample"), &unsupported_slot, Division::Below);

        assert!(matches!(failure, Err(HostError::Absent(_))));
        assert!(
            Panes::at(&bench.window, &old).is_some(),
            "the old slot remains addressable"
        );
        assert_eq!(interface.parent(), before, "the same holder still owns the interface");
        assert_eq!(
            Panes::all(&bench.window)
                .iter()
                .filter(|pane| pane.occupant == Occupant::Surface)
                .count(),
            1,
            "failure creates no placeholder surface"
        );
        drop(first);
    }

    pub(super) fn a_restored_surface_without_its_extension_is_frozen_rather_than_a_shell() {
        let bench = Bench::new();
        Window::exhibit(&bench.window, Gallery::new());
        let storage = tempfile::tempdir().expect("temporary directory");
        let node = PaneNode::Surface(SurfacePane {
            extension: "departed".to_owned(),
            provider: None,
            slot: Some("7".to_owned()),
        });

        let mut pids = Vec::new();
        let (widget, terminal) = WindowSession::new(&bench.window).build_pane_widget(
            &node,
            storage.path(),
            &mut pids,
            &ProductionPaneLauncher,
        );
        bench.page.append(&widget);

        assert!(terminal.is_none(), "an absent extension is never replaced by a shell");
        assert!(
            super::descendants(&widget)
                .iter()
                .any(|found| found.has_css_class(ABSENCE)),
            "the pane says whose interface belongs in it and that nobody is drawing"
        );
        let held = Panes::at(&bench.window, "7").expect("the restored pane keeps its slot");
        assert_eq!(held.occupant, Occupant::Surface);
    }
}
