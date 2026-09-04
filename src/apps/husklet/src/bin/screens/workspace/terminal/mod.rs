pub(crate) struct TermWin {
    stack: gtk::Stack,
    tabs: gtk::Box,
    pub(crate) ws: WorkspaceConfig,
    focused: RefCell<Option<vte4::Terminal>>,
    /// Last terminal focused in each tab, so returning to a split restores the
    /// pane the user was working in rather than arbitrarily choosing its first.
    page_focus: RefCell<HashMap<String, glib::WeakRef<vte4::Terminal>>>,
    entries: RefCell<Vec<TabEntry>>,
    pids: RefCell<HashMap<String, Vec<Rc<Cell<i32>>>>>,
    counter: Cell<u32>,
    shell_no: Cell<u32>,
    /// Monotonic stable identity allocator for persisted pane layouts.
    pub(crate) slot_ctr: Cell<u32>,
    /// Registry of every live pane: its terminal (weak), layout slot, and worker pid.
    pub(crate) panes: RefCell<Vec<PaneRegistration>>,
    /// Registry of every live pane holding an extension's interface instead of
    /// a shell. Kept apart from `panes` because a surface has no terminal, and
    /// a registry of one thing that is sometimes another is a registry nobody
    /// can read.
    pub(crate) surfaces: RefCell<Vec<SurfaceRegistration>>,
    pub(crate) displaced: RefCell<HashMap<String, vte4::Terminal>>,
    /// Where an extension's interface widget is found, so a pane can hold the
    /// one that already exists rather than starting a second of it.
    gallery: RefCell<Option<screens::workspace::extensions::Gallery>>,
    /// Slim Cmd+F search bar over the focused terminal.
    search: Search,
    zoom: Zoom,
    /// Keyboard scrollback-navigation ("copy") mode is active.
    copymode: CopyMode,
    /// The window is closing; child exits should not mutate the saved layout during teardown.
    closing: Cell<bool>,
    overview_page: Option<screens::workspace::Page>,
    observers: RefCell<Vec<hl::extension::WeakEvents>>,
    last_pointer: RefCell<Option<PointerTarget>>,
}

#[derive(Clone)]
struct PointerTarget {
    slot: String,
    generation: u64,
    x: f64,
    y: f64,
}

const POINTER_NUMBER_LIMIT: f64 = 1_000_000.0;

fn bounded_pointer_number(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(-POINTER_NUMBER_LIMIT, POINTER_NUMBER_LIMIT)
    } else {
        0.0
    }
}

pub(crate) struct PaneRegistration {
    terminal: glib::WeakRef<vte4::Terminal>,
    slot: String,
}

impl PaneRegistration {
    pub(crate) fn new(terminal: &vte4::Terminal, slot: String) -> Self {
        Self {
            terminal: terminal.downgrade(),
            slot,
        }
    }
}
/// One live pane holding an extension's interface.
pub(crate) struct SurfaceRegistration {
    widget: glib::WeakRef<gtk::Widget>,
    slot: String,
    /// The extension whose interface belongs in this pane, which is what a
    /// restored layout has to name.
    extension: String,
    provider: Option<String>,
}

impl SurfaceRegistration {
    pub(crate) fn new(widget: &gtk::Widget, slot: String, extension: String, provider: Option<String>) -> Self {
        Self {
            widget: widget.downgrade(),
            slot,
            extension,
            provider,
        }
    }
}

pub(crate) struct TabEntry {
    name: String,
    button: gtk::Box,
    title: gtk::Label,
    persisted: bool,
}

/// The minimalist search bar: a slim black overlay with a query field + a match-state hint.
pub(crate) struct Search {
    bar: gtk::Box,
    entry: gtk::Entry,
    info: gtk::Label,
    caseless: Cell<bool>,
}

pub(crate) struct Window;

pub(crate) struct Clipboard;

pub(crate) struct SplitAction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shortcut {
    Tab,
    Close,
    Split(bool),
    Search,
    CopyMode,
    Copy,
    Cut,
    Paste,
    SelectAll,
    ZoomIn,
    ZoomOut,
    ZoomReset,
}

const ZOOM_MIN: f64 = 0.5;
const ZOOM_MAX: f64 = 3.0;
const ZOOM_STEP: f64 = 0.1;

pub(crate) struct Zoom(Cell<f64>);

impl Zoom {
    fn new() -> Self {
        Self(Cell::new(1.0))
    }

    pub(crate) fn scale(&self) -> f64 {
        self.0.get()
    }

    fn adjust(&self, delta: f64) -> f64 {
        let scale = (self.scale() + delta).clamp(ZOOM_MIN, ZOOM_MAX);
        self.0.set(scale);
        scale
    }

    fn reset(&self) -> f64 {
        self.0.set(1.0);
        1.0
    }
}

impl Shortcut {
    #[cfg(target_os = "macos")]
    fn from_key(key: gdk::Key, state: gdk::ModifierType) -> Option<Self> {
        if !state.contains(gdk::ModifierType::META_MASK) {
            return None;
        }
        match key {
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => return Some(Self::ZoomIn),
            gdk::Key::minus | gdk::Key::KP_Subtract => return Some(Self::ZoomOut),
            gdk::Key::_0 | gdk::Key::KP_0 => return Some(Self::ZoomReset),
            _ => {}
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        match key {
            gdk::Key::t | gdk::Key::T => Some(Self::Tab),
            gdk::Key::w | gdk::Key::W => Some(Self::Close),
            gdk::Key::d | gdk::Key::D => Some(Self::Split(shift)),
            gdk::Key::f | gdk::Key::F => Some(Self::Search),
            gdk::Key::c | gdk::Key::C if shift => Some(Self::CopyMode),
            gdk::Key::c | gdk::Key::C => Some(Self::Copy),
            gdk::Key::x | gdk::Key::X => Some(Self::Cut),
            gdk::Key::v | gdk::Key::V => Some(Self::Paste),
            gdk::Key::a | gdk::Key::A => Some(Self::SelectAll),
            _ => None,
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn from_key(key: gdk::Key, state: gdk::ModifierType) -> Option<Self> {
        if !state.contains(gdk::ModifierType::CONTROL_MASK) {
            return None;
        }
        match key {
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => return Some(Self::ZoomIn),
            gdk::Key::minus | gdk::Key::KP_Subtract => return Some(Self::ZoomOut),
            gdk::Key::_0 | gdk::Key::KP_0 => return Some(Self::ZoomReset),
            _ => {}
        }
        if !state.contains(gdk::ModifierType::SHIFT_MASK) {
            return None;
        }
        let alternate = state.contains(gdk::ModifierType::ALT_MASK);
        match key {
            gdk::Key::t | gdk::Key::T => Some(Self::Tab),
            gdk::Key::w | gdk::Key::W => Some(Self::Close),
            gdk::Key::d | gdk::Key::D => Some(Self::Split(alternate)),
            gdk::Key::f | gdk::Key::F => Some(Self::Search),
            gdk::Key::c | gdk::Key::C if alternate => Some(Self::CopyMode),
            gdk::Key::c | gdk::Key::C => Some(Self::Copy),
            gdk::Key::x | gdk::Key::X => Some(Self::Cut),
            gdk::Key::v | gdk::Key::V => Some(Self::Paste),
            gdk::Key::a | gdk::Key::A => Some(Self::SelectAll),
            _ => None,
        }
    }
}

fn copy_mode_captures(active: bool, shortcut: Option<Shortcut>) -> bool {
    active && shortcut.is_none()
}

fn editable_captures(focused: bool, shortcut: Option<Shortcut>) -> bool {
    focused
        && matches!(
            shortcut,
            Some(Shortcut::Copy | Shortcut::Cut | Shortcut::Paste | Shortcut::SelectAll)
        )
}

impl SplitAction {
    pub(crate) fn focused(window: &Rc<TermWin>, vertical: bool) {
        let Some(pane) = PaneChooser::selected(window) else {
            return;
        };
        let terminal = pane
            .content
            .clone()
            .downcast::<vte4::Terminal>()
            .ok()
            .or_else(|| window.displaced.borrow().get(&pane.slot).cloned());
        let Some(terminal) = terminal else { return };
        let orientation = if vertical {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        };
        PaneView::split_at(window, &pane, &terminal, orientation);
    }
}

impl TermWin {
    pub(crate) fn observer(&self) -> hl::extension::Events {
        let events = hl::extension::Events::default();
        self.observers.borrow_mut().push(events.downgrade());
        events
    }

    fn broadcast(&self, event: hl_extension::WorkspaceEvent) {
        self.observers
            .borrow_mut()
            .retain(|observer| observer.observe(event.clone()));
    }

    fn focused_event_identity(window: &Rc<Self>, focus: Option<gtk::Widget>) -> (Option<String>, Option<u64>) {
        let mut widget = focus;
        let terminal = loop {
            let Some(current) = widget else { return (None, None) };
            if let Ok(terminal) = current.clone().downcast::<vte4::Terminal>() {
                break terminal;
            }
            widget = current.parent();
        };
        let slot = Slots::new(window).of(&terminal);
        let generation = slot.as_ref().map(|_| 0);
        (slot, generation)
    }

    fn pointer_target(window: &Rc<Self>, x: f64, y: f64) -> Option<PointerTarget> {
        let mut widget = window.stack.pick(x, y, gtk::PickFlags::DEFAULT)?;
        while !PaneChrome::is(&widget) {
            widget = widget.parent()?;
        }
        let pane = Panes::all(window).into_iter().find(|pane| pane.widget == widget)?;
        let point = window
            .stack
            .compute_point(&pane.widget, &gtk::graphene::Point::new(x as f32, y as f32))?;
        let generation = Slots::new(window)
            .surface(&pane.content)
            .and_then(|(_, extension, _)| Window::gallery(window)?.generation(&extension))
            .unwrap_or(0);
        Some(PointerTarget {
            slot: pane.slot,
            generation,
            x: bounded_pointer_number(f64::from(point.x())),
            y: bounded_pointer_number(f64::from(point.y())),
        })
    }

    fn pointer_event(
        target: &PointerTarget,
        phase: hl_extension::PointerPhase,
        button: Option<u32>,
        modifiers: gtk::gdk::ModifierType,
        delta: Option<(f64, f64)>,
    ) -> hl_extension::WorkspaceEvent {
        hl_extension::WorkspaceEvent::Pointer {
            phase,
            slot: target.slot.clone(),
            generation: target.generation,
            x: target.x,
            y: target.y,
            button,
            modifiers: modifier_names(modifiers),
            delta_x: delta.map(|(x, _)| bounded_pointer_number(x)),
            delta_y: delta.map(|(_, y)| bounded_pointer_number(y)),
        }
    }

    fn apply_zoom(&self, scale: f64) {
        self.panes.borrow_mut().retain(|pane| {
            let Some(terminal) = pane.terminal.upgrade() else {
                return false;
            };
            terminal.set_font_scale(scale);
            true
        });
    }
}

fn modifier_names(state: gtk::gdk::ModifierType) -> Vec<String> {
    [
        (gtk::gdk::ModifierType::SHIFT_MASK, "shift"),
        (gtk::gdk::ModifierType::CONTROL_MASK, "control"),
        (gtk::gdk::ModifierType::ALT_MASK, "alt"),
        (gtk::gdk::ModifierType::META_MASK, "meta"),
        (gtk::gdk::ModifierType::SUPER_MASK, "super"),
    ]
    .into_iter()
    .filter_map(|(mask, name)| state.contains(mask).then(|| name.to_owned()))
    .collect()
}

impl Clipboard {
    pub(crate) fn copy_selection(window: &TermWin) -> glib::Propagation {
        let focused = window.focused.borrow();
        if let Some(terminal) = focused.as_ref().filter(|terminal| terminal.has_selection()) {
            terminal.copy_clipboard_format(vte4::Format::Text);
        }
        glib::Propagation::Stop
    }

    pub(crate) fn paste(window: &TermWin) -> glib::Propagation {
        if let Some(terminal) = window.focused.borrow().as_ref() {
            terminal.paste_clipboard();
        }
        glib::Propagation::Stop
    }

    pub(crate) fn select_all(window: &TermWin) -> glib::Propagation {
        if let Some(terminal) = window.focused.borrow().as_ref() {
            terminal.select_all();
        }
        glib::Propagation::Stop
    }
}

impl Window {
    #[cfg(test)]
    pub(crate) fn pointer_test_point(window: &Rc<TermWin>, slot: &str) -> Option<(f64, f64)> {
        let pane = Panes::at(window, slot)?;
        let point = pane.widget.compute_point(
            &window.stack,
            &gtk::graphene::Point::new(pane.widget.width() as f32 / 2.0, pane.widget.height() as f32 / 2.0),
        )?;
        Some((f64::from(point.x()), f64::from(point.y())))
    }

    #[cfg(test)]
    pub(crate) fn pointer_test_target(window: &Rc<TermWin>, x: f64, y: f64) -> Option<(String, u64, f64, f64)> {
        TermWin::pointer_target(window, x, y).map(|target| (target.slot, target.generation, target.x, target.y))
    }

    /// Every tab, as its name, its widget, and the pane slots inside it.
    ///
    /// The terminal window is the only thing that knows this, and an extension
    /// asking what it may split has to be told from here rather than from a
    /// second registry that could disagree with the widgets.
    pub(crate) fn tabs(window: &Rc<TermWin>) -> Vec<(String, gtk::Widget, Vec<String>)> {
        let names: Vec<String> = window.entries.borrow().iter().map(|entry| entry.name.clone()).collect();
        names
            .into_iter()
            .filter_map(|name| window.stack.child_by_name(&name).map(|widget| (name, widget)))
            .map(|(name, widget)| {
                let slots = Self::slots(window, &widget);
                (name, widget, slots)
            })
            .collect()
    }

    /// The layout slots of every pane under one tab's widget, shells and
    /// extension surfaces alike, in the order they are laid out.
    pub(crate) fn slots(window: &Rc<TermWin>, widget: &gtk::Widget) -> Vec<String> {
        Panes::under(window, widget)
            .into_iter()
            .map(|occupancy| occupancy.slot)
            .collect()
    }

    /// The terminal registered under one slot, if it is still open.
    ///
    /// A surface pane answers `None`: it holds no shell, and a caller asking to
    /// type into one is asking for something that is not there.
    pub(crate) fn pane(window: &Rc<TermWin>, slot: &str) -> Option<vte4::Terminal> {
        Panes::at(window, slot)?.content.downcast::<vte4::Terminal>().ok()
    }

    /// A fresh pane identity, for a pane this window is about to build.
    pub(crate) fn slot(window: &Rc<TermWin>) -> String {
        Slots::new(window).allocate()
    }

    /// Where this window finds the extension interfaces its panes may hold.
    pub(crate) fn exhibit(window: &Rc<TermWin>, gallery: screens::workspace::extensions::Gallery) {
        *window.gallery.borrow_mut() = Some(gallery);
    }

    /// The gallery this window was given, if the workspace shell offered one.
    pub(crate) fn gallery(window: &Rc<TermWin>) -> Option<screens::workspace::extensions::Gallery> {
        window.gallery.borrow().clone()
    }

    pub(crate) fn tab_title(window: &Rc<TermWin>, name: &str) -> Option<String> {
        window
            .entries
            .borrow()
            .iter()
            .find(|entry| entry.name == name)
            .map(TabEntry::title)
    }

    pub(crate) fn retitle_pane(window: &Rc<TermWin>, pane: &gtk::Widget, title: &str) -> bool {
        let Some(page) = Page::of(window, pane) else {
            return false;
        };
        let mut entries = window.entries.borrow_mut();
        let Some(entry) = entries.iter_mut().find(|entry| entry.name == page.name()) else {
            return false;
        };
        entry.retitle(title);
        true
    }

    pub(crate) fn active_tab(window: &Rc<TermWin>) -> Option<String> {
        window.stack.visible_child_name().map(|name| name.to_string())
    }

    /// A window with no application behind it, for scenarios about panes.
    ///
    /// Every pane operation is about the widget tree and the two registries,
    /// none of which needs a presented window or a running workspace. Building
    /// one here is what lets those be exercised without starting shells.
    #[cfg(test)]
    pub(crate) fn bench(ws: &WorkspaceConfig) -> Rc<TermWin> {
        let stack = gtk::Stack::new();
        stack.set_vhomogeneous(false);
        stack.set_hhomogeneous(false);
        // Presented, because a terminal only takes in what is fed to it once it
        // has been realized, and a pane nobody can read is not a pane.
        let presented = gtk::Window::builder().title(&ws.name).child(&stack).build();
        presented.present();
        Rc::new(TermWin {
            zoom: Zoom::new(),
            stack,
            tabs: gtk::Box::new(gtk::Orientation::Horizontal, 0),
            ws: ws.clone(),
            focused: RefCell::new(None),
            page_focus: RefCell::new(HashMap::new()),
            entries: RefCell::new(Vec::new()),
            pids: RefCell::new(HashMap::new()),
            counter: Cell::new(0),
            shell_no: Cell::new(0),
            slot_ctr: Cell::new(0),
            panes: RefCell::new(Vec::new()),
            surfaces: RefCell::new(Vec::new()),
            displaced: RefCell::new(HashMap::new()),
            gallery: RefCell::new(None),
            search: Search::new(),
            copymode: CopyMode::new(),
            closing: Cell::new(false),
            overview_page: None,
            observers: RefCell::new(Vec::new()),
            last_pointer: RefCell::new(None),
        })
    }

    pub(crate) fn open(app: &gtk::Application, ws: &WorkspaceConfig) {
        Self::open_page(app, ws, None);
    }

    pub(crate) fn settings(app: &gtk::Application, ws: &WorkspaceConfig) {
        Self::open_page(app, ws, Some(screens::workspace::Page::Workspace));
    }

    pub(crate) fn open_page(
        app: &gtk::Application,
        ws: &WorkspaceConfig,
        overview_page: Option<screens::workspace::Page>,
    ) {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title(&ws.name)
            .default_width(1040)
            .default_height(680)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Full-width tab strip: a homogeneous box so tabs are EXACTLY equal width (100/50/33/25…) and fill
        // the entire width. No `+` button — new tabs come from ⌘T — so nothing eats into the tab widths.
        let tabbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabbar.add_css_class("tabbar");
        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabs.set_homogeneous(true);
        tabs.set_hexpand(true);
        tabbar.append(&tabs);

        let stack = gtk::Stack::new();
        stack.add_css_class("pages");
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        // Size to the visible child (a terminal), NOT the tallest child — otherwise the grid is capped.
        stack.set_vhomogeneous(false);
        stack.set_hhomogeneous(false);
        stack.set_transition_type(gtk::StackTransitionType::None);

        // The search bar floats over the terminal stack via an Overlay (top-right, hidden until Cmd+F).
        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        overlay.set_hexpand(true);
        overlay.set_child(Some(&stack));
        let search = Search::new();
        overlay.add_overlay(&search.bar);

        root.append(&tabbar);
        root.append(&overlay);

        let tw = Rc::new(TermWin {
            stack,
            tabs,
            ws: ws.clone(),
            focused: RefCell::new(None),
            page_focus: RefCell::new(HashMap::new()),
            entries: RefCell::new(Vec::new()),
            pids: RefCell::new(HashMap::new()),
            counter: Cell::new(0),
            shell_no: Cell::new(0),
            slot_ctr: Cell::new(0),
            panes: RefCell::new(Vec::new()),
            surfaces: RefCell::new(Vec::new()),
            displaced: RefCell::new(HashMap::new()),
            gallery: RefCell::new(None),
            search,
            zoom: Zoom::new(),
            copymode: CopyMode::new(),
            closing: Cell::new(false),
            overview_page,
            observers: RefCell::new(Vec::new()),
            last_pointer: RefCell::new(None),
        });
        Search::wire(&tw);

        let keys = gtk::EventControllerKey::new();
        // CAPTURE phase so ⌘-shortcuts are handled by the window BEFORE the focused VTE swallows them
        // (otherwise ⌘T/⌘D just type into the terminal).
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let tw = tw.clone();
            let root = window.clone();
            keys.connect_key_pressed(move |_, key, _c, state| {
                let (slot, generation) = TermWin::focused_event_identity(&tw, gtk::prelude::RootExt::focus(&root));
                tw.broadcast(hl_extension::WorkspaceEvent::Key {
                    key: key
                        .name()
                        .map_or_else(String::new, |name| name.chars().take(64).collect()),
                    modifiers: modifier_names(state),
                    pressed: true,
                    slot,
                    generation,
                });
                let shortcut = Shortcut::from_key(key, state);
                // Window shortcuts are captured before the focused widget sees them. Text-editing
                // commands must remain with the search entry; redirecting Paste to the last VTE can
                // execute clipboard contents in the shell while the user is entering a query.
                if editable_captures(tw.search.entry.has_focus(), shortcut) {
                    return glib::Propagation::Proceed;
                }
                // Copy/scroll mode intercepts plain (unmodified) keys for keyboard scrollback navigation.
                if copy_mode_captures(tw.copymode.is_active(), shortcut) && tw.copymode.key(&tw, key, state) {
                    return glib::Propagation::Stop;
                }
                match shortcut {
                    Some(Shortcut::Tab) => {
                        Tabs::new(&tw).terminal();
                        glib::Propagation::Stop
                    }
                    Some(Shortcut::Close) => {
                        CurrentPage::close(&tw);
                        glib::Propagation::Stop
                    }
                    Some(Shortcut::Split(vertical)) => {
                        SplitAction::focused(&tw, vertical);
                        glib::Propagation::Stop
                    }
                    Some(Shortcut::Search) => {
                        tw.search.toggle(tw.focused.borrow().clone());
                        glib::Propagation::Stop
                    }
                    Some(Shortcut::CopyMode) => {
                        tw.copymode.enter(tw.focused.borrow().clone());
                        glib::Propagation::Stop
                    }
                    Some(Shortcut::Copy | Shortcut::Cut) => Clipboard::copy_selection(&tw),
                    Some(Shortcut::Paste) => Clipboard::paste(&tw),
                    Some(Shortcut::SelectAll) => Clipboard::select_all(&tw),
                    Some(Shortcut::ZoomIn) => {
                        let scale = tw.zoom.adjust(ZOOM_STEP);
                        tw.apply_zoom(scale);
                        glib::Propagation::Stop
                    }
                    Some(Shortcut::ZoomOut) => {
                        let scale = tw.zoom.adjust(-ZOOM_STEP);
                        tw.apply_zoom(scale);
                        glib::Propagation::Stop
                    }
                    Some(Shortcut::ZoomReset) => {
                        let scale = tw.zoom.reset();
                        tw.apply_zoom(scale);
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                }
            });
        }
        {
            let tw = tw.clone();
            let root = window.clone();
            keys.connect_key_released(move |_, key, _c, state| {
                let (slot, generation) = TermWin::focused_event_identity(&tw, gtk::prelude::RootExt::focus(&root));
                tw.broadcast(hl_extension::WorkspaceEvent::Key {
                    key: key
                        .name()
                        .map_or_else(String::new, |name| name.chars().take(64).collect()),
                    modifiers: modifier_names(state),
                    pressed: false,
                    slot,
                    generation,
                });
            });
        }
        window.add_controller(keys);

        let motion = gtk::EventControllerMotion::new();
        {
            let tw = tw.clone();
            motion.connect_motion(move |controller, x, y| {
                let Some(target) = TermWin::pointer_target(&tw, x, y) else {
                    return;
                };
                let previous = tw.last_pointer.replace(Some(target.clone()));
                if let Some(previous) =
                    previous.filter(|previous| previous.slot != target.slot || previous.generation != target.generation)
                {
                    tw.broadcast(TermWin::pointer_event(
                        &previous,
                        hl_extension::PointerPhase::Leave,
                        None,
                        controller.current_event_state(),
                        None,
                    ));
                    tw.broadcast(TermWin::pointer_event(
                        &target,
                        hl_extension::PointerPhase::Enter,
                        None,
                        controller.current_event_state(),
                        None,
                    ));
                }
                tw.broadcast(TermWin::pointer_event(
                    &target,
                    hl_extension::PointerPhase::Move,
                    None,
                    controller.current_event_state(),
                    None,
                ));
            });
        }
        {
            let tw = tw.clone();
            motion.connect_enter(move |controller, x, y| {
                let Some(target) = TermWin::pointer_target(&tw, x, y) else {
                    return;
                };
                tw.broadcast(TermWin::pointer_event(
                    &target,
                    hl_extension::PointerPhase::Enter,
                    None,
                    controller.current_event_state(),
                    None,
                ));
                tw.last_pointer.replace(Some(target));
            });
        }
        {
            let tw = tw.clone();
            motion.connect_leave(move |controller| {
                if let Some(target) = tw.last_pointer.borrow_mut().take() {
                    tw.broadcast(TermWin::pointer_event(
                        &target,
                        hl_extension::PointerPhase::Leave,
                        None,
                        controller.current_event_state(),
                        None,
                    ));
                }
            });
        }
        tw.stack.add_controller(motion);

        let clicks = gtk::GestureClick::new();
        clicks.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let tw = tw.clone();
            clicks.connect_pressed(move |gesture, _, x, y| {
                let Some(target) = TermWin::pointer_target(&tw, x, y) else {
                    return;
                };
                tw.broadcast(TermWin::pointer_event(
                    &target,
                    hl_extension::PointerPhase::Press,
                    Some(gesture.current_button()),
                    gesture.current_event_state(),
                    None,
                ));
                tw.last_pointer.replace(Some(target));
            });
        }
        {
            let tw = tw.clone();
            clicks.connect_released(move |gesture, _, x, y| {
                let Some(target) = TermWin::pointer_target(&tw, x, y) else {
                    return;
                };
                let button = gesture.current_button();
                tw.broadcast(TermWin::pointer_event(
                    &target,
                    hl_extension::PointerPhase::Release,
                    Some(button),
                    gesture.current_event_state(),
                    None,
                ));
                tw.broadcast(TermWin::pointer_event(
                    &target,
                    if button == 3 {
                        hl_extension::PointerPhase::Context
                    } else {
                        hl_extension::PointerPhase::Click
                    },
                    Some(button),
                    gesture.current_event_state(),
                    None,
                ));
            });
        }
        tw.stack.add_controller(clicks);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        {
            let tw = tw.clone();
            scroll.connect_scroll(move |controller, dx, dy| {
                let target = controller
                    .current_event()
                    .and_then(|event| event.position())
                    .and_then(|(x, y)| TermWin::pointer_target(&tw, x, y));
                if let Some(target) = target {
                    tw.broadcast(TermWin::pointer_event(
                        &target,
                        hl_extension::PointerPhase::Scroll,
                        None,
                        controller.current_event_state(),
                        Some((dx, dy)),
                    ));
                }
                glib::Propagation::Proceed
            });
        }
        tw.stack.add_controller(scroll);
        {
            let tw = tw.clone();
            window.connect_is_active_notify(move |window| {
                let (slot, generation) = TermWin::focused_event_identity(&tw, gtk::prelude::RootExt::focus(window));
                tw.broadcast(hl_extension::WorkspaceEvent::Focus {
                    active: window.is_active(),
                    slot,
                    generation,
                })
            });
        }

        CloseRequest::install(&window, &tw);

        Tabs::new(&tw).overview();
        // Restore the saved session (tabs + splits + per-pane history) if this workspace has one; else open a
        // single fresh shell. The debug hooks below still layer on top.
        match Session::open(&tw.ws.storage_dir(&Home::current().root())) {
            Ok(saved) if !saved.tabs.is_empty() => WindowSession::new(&tw).restore(&saved),
            Ok(_) => drop(Tabs::new(&tw).terminal()),
            Err(error) => {
                hl_log::hl_error!(
                    hl_log::tag::RUNTIME,
                    "failed to restore terminal session for {}: {error}",
                    tw.ws.name
                );
                Tabs::new(&tw).terminal();
                if let Some(terminal) = tw.stack.visible_child().and_then(|page| PaneView::first(&page)) {
                    terminal.feed(
                        format!(
                            "\r\n\x1b[31mworkspace restore incomplete: layout/history could not be restored: {error}\x1b[0m\r\n"
                        )
                        .as_bytes(),
                    );
                }
            }
        }
        // Debug: HL_TERM_TABS=N opens N total shell tabs (to verify exact equal-width tabs).
        if let Some(n) = AppConfig::get().tabs {
            for _ in 1..n {
                Tabs::new(&tw).terminal();
            }
        }
        // Debug: HL_TERM_SPLIT=h|v splits the current shell tab (to screenshot the split separator).
        if let Some(dir) = AppConfig::get().split.as_deref() {
            if let Some(t) = tw.stack.visible_child().and_then(|c| PaneView::first(&c)) {
                *tw.focused.borrow_mut() = Some(t.clone());
                let o = if dir == "v" {
                    gtk::Orientation::Vertical
                } else {
                    gtk::Orientation::Horizontal
                };
                PaneView::new(&tw, &t).split(o);
            }
        }
        // Debug: HL_TERM_OVERVIEW selects the overview (first) tab for screenshotting.
        if AppConfig::get().overview {
            let first = tw.entries.borrow().first().map(|e| e.name.clone());
            if let Some(n) = first {
                Page::new(&tw, &n).select();
            }
        }

        window.set_child(Some(&root));
        window.present();
        host::appearance::Appearance::apply();
        Screenshot::schedule(&window, "terminal");
        Screenshot::schedule_resize(&window);
        LiveActions::schedule(&tw);
    }
}

// -------------------------------------------------------------------------------------------------
// Search bar (Cmd+F) — minimalist, highlights matches via VTE's search API.
// -------------------------------------------------------------------------------------------------

pub(crate) struct Terminal<'a>(pub(crate) &'a vte4::Terminal);

impl<'a> Terminal<'a> {
    pub(crate) fn new(terminal: &'a vte4::Terminal) -> Self {
        Self(terminal)
    }

    /// The terminal's current directory, decoded from OSC 7's `file://` URI.
    pub(crate) fn working_directory(&self) -> Option<String> {
        let uri = self.0.current_directory_uri()?;
        session::WorkingDirectory::from_osc7(&uri).map(hl_ws_term::WorkingDirectory::into_string)
    }
}

use crate::*;

mod actions;
mod close;
mod grid;
mod launch;
mod link;
mod pane;
mod panes;
#[cfg(test)]
mod restore_profile;
mod search;
mod slots;
mod state;
mod surface;
mod text;

use actions::LiveActions;
pub(crate) use close::*;
pub(crate) use launch::*;
pub(crate) use pane::*;
pub(crate) use panes::*;
pub(crate) use slots::*;
pub(crate) use state::*;
pub(crate) use surface::*;

#[cfg(test)]
mod shortcut_tests {
    use super::{editable_captures, Shortcut};
    use gtk::gdk;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_edit_shortcuts_never_fall_through_to_vte() {
        let command = gdk::ModifierType::META_MASK;
        assert_eq!(Shortcut::from_key(gdk::Key::c, command), Some(Shortcut::Copy));
        assert_eq!(Shortcut::from_key(gdk::Key::x, command), Some(Shortcut::Cut));
        assert_eq!(Shortcut::from_key(gdk::Key::v, command), Some(Shortcut::Paste));
        assert_eq!(Shortcut::from_key(gdk::Key::a, command), Some(Shortcut::SelectAll));
        assert_eq!(Shortcut::from_key(gdk::Key::c, gdk::ModifierType::empty()), None);
        assert_eq!(Shortcut::from_key(gdk::Key::plus, command), Some(Shortcut::ZoomIn));
        assert_eq!(Shortcut::from_key(gdk::Key::minus, command), Some(Shortcut::ZoomOut));
        assert_eq!(Shortcut::from_key(gdk::Key::_0, command), Some(Shortcut::ZoomReset));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_shortcuts_preserve_terminal_control_keys() {
        let control = gdk::ModifierType::CONTROL_MASK;
        let command = control | gdk::ModifierType::SHIFT_MASK;
        assert_eq!(Shortcut::from_key(gdk::Key::c, control), None);
        assert_eq!(Shortcut::from_key(gdk::Key::c, command), Some(Shortcut::Copy));
        assert_eq!(Shortcut::from_key(gdk::Key::v, command), Some(Shortcut::Paste));
        assert_eq!(Shortcut::from_key(gdk::Key::t, command), Some(Shortcut::Tab));
        assert_eq!(Shortcut::from_key(gdk::Key::w, command), Some(Shortcut::Close));
        assert_eq!(Shortcut::from_key(gdk::Key::plus, control), Some(Shortcut::ZoomIn));
        assert_eq!(Shortcut::from_key(gdk::Key::minus, control), Some(Shortcut::ZoomOut));
        assert_eq!(Shortcut::from_key(gdk::Key::_0, control), Some(Shortcut::ZoomReset));
        assert!(!super::copy_mode_captures(
            true,
            Shortcut::from_key(gdk::Key::minus, control)
        ));
        assert!(!super::copy_mode_captures(
            true,
            Shortcut::from_key(gdk::Key::_0, control)
        ));
        assert!(super::copy_mode_captures(
            true,
            Shortcut::from_key(gdk::Key::c, control)
        ));
    }

    #[test]
    fn zoom_clamps_and_resets_around_the_configured_font() {
        let zoom = super::Zoom::new();
        for _ in 0..100 {
            zoom.adjust(super::ZOOM_STEP);
        }
        assert!((zoom.scale() - super::ZOOM_MAX).abs() < f64::EPSILON);
        for _ in 0..100 {
            zoom.adjust(-super::ZOOM_STEP);
        }
        assert!((zoom.scale() - super::ZOOM_MIN).abs() < f64::EPSILON);
        assert!((zoom.reset() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn focused_editable_keeps_only_text_editing_shortcuts() {
        for shortcut in [Shortcut::Copy, Shortcut::Cut, Shortcut::Paste, Shortcut::SelectAll] {
            assert!(editable_captures(true, Some(shortcut)));
            assert!(!editable_captures(false, Some(shortcut)));
        }
        for shortcut in [Shortcut::Tab, Shortcut::ZoomIn, Shortcut::ZoomOut, Shortcut::ZoomReset] {
            assert!(!editable_captures(true, Some(shortcut)));
        }
        assert!(!editable_captures(true, None));
    }
}

#[cfg(test)]
mod workspace_event_identity_tests {
    use super::*;

    #[test]
    fn focused_terminal_identity_is_reported_and_search_focus_is_explicitly_absent() {
        assert!(
            crate::test_support::on_the_toolkit_thread(|| {
                let workspace = WorkspaceConfig::new("event-identity", "offline.invalid", hl_ws::Arch::Amd64);
                let tw = Window::bench(&workspace);
                let terminal = vte4::Terminal::new();
                Slots::new(&tw).hold(&terminal, "pane-stable".into());
                assert_eq!(
                    TermWin::focused_event_identity(&tw, Some(terminal.clone().upcast())),
                    (Some("pane-stable".into()), Some(0))
                );

                assert_eq!(
                    TermWin::focused_event_identity(&tw, Some(tw.search.entry.clone().upcast())),
                    (None, None)
                );
            }),
            "workspace event identity requires an Xvfb display"
        );
    }
}
