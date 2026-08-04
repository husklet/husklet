pub(crate) struct TermWin {
    stack: gtk::Stack,
    tabs: gtk::Box,
    pub(crate) ws: WorkspaceConfig,
    focused: RefCell<Option<vte4::Terminal>>,
    entries: RefCell<Vec<TabEntry>>,
    pids: RefCell<HashMap<String, Vec<Rc<Cell<i32>>>>>,
    counter: Cell<u32>,
    shell_no: Cell<u32>,
    /// Monotonic stable identity allocator for persisted pane layouts.
    pub(crate) slot_ctr: Cell<u32>,
    /// Registry of every live pane: its terminal (weak), layout slot, and worker pid.
    pub(crate) panes: RefCell<Vec<PaneRegistration>>,
    /// Slim Cmd+F search bar over the focused terminal.
    search: Search,
    /// Keyboard scrollback-navigation ("copy") mode is active.
    copymode: CopyMode,
    /// The window is closing; child exits should not mutate the saved layout during teardown.
    closing: Cell<bool>,
    overview_page: Option<screens::workspace::Page>,
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
pub(crate) struct TabEntry {
    name: String,
    button: gtk::Box,
}

/// The minimalist search bar: a slim black overlay with a query field + a match-state hint.
pub(crate) struct Search {
    bar: gtk::Box,
    entry: gtk::Entry,
    info: gtk::Label,
    caseless: Cell<bool>,
}

pub(crate) struct TerminalWindow;

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
}

impl Shortcut {
    fn from_key(key: gdk::Key, state: gdk::ModifierType) -> Option<Self> {
        if !state.contains(gdk::ModifierType::META_MASK) {
            return None;
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
}

impl SplitAction {
    pub(crate) fn focused(window: &Rc<TermWin>, vertical: bool) {
        let Some(terminal) = window.focused.borrow().clone() else {
            return;
        };
        let orientation = if vertical {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        };
        TerminalPane::new(window, &terminal).split(orientation);
    }
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

impl TerminalWindow {
    pub(crate) fn open(app: &gtk::Application, ws: &WorkspaceConfig) {
        Self::open_page(app, ws, None);
    }

    pub(crate) fn settings(app: &gtk::Application, ws: &WorkspaceConfig) {
        Self::open_page(app, ws, Some(screens::workspace::Page::Settings));
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
            entries: RefCell::new(Vec::new()),
            pids: RefCell::new(HashMap::new()),
            counter: Cell::new(0),
            shell_no: Cell::new(0),
            slot_ctr: Cell::new(0),
            panes: RefCell::new(Vec::new()),
            search,
            copymode: CopyMode::new(),
            closing: Cell::new(false),
            overview_page,
        });
        tw.search.wire(&tw);

        let keys = gtk::EventControllerKey::new();
        // CAPTURE phase so ⌘-shortcuts are handled by the window BEFORE the focused VTE swallows them
        // (otherwise ⌘T/⌘D just type into the terminal).
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let tw = tw.clone();
            keys.connect_key_pressed(move |_, key, _c, state| {
                // Copy/scroll mode intercepts plain (unmodified) keys for keyboard scrollback navigation.
                if tw.copymode.is_active()
                    && !state.contains(gdk::ModifierType::META_MASK)
                    && tw.copymode.key(&tw, key, state)
                {
                    return glib::Propagation::Stop;
                }
                match Shortcut::from_key(key, state) {
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
                    None => glib::Propagation::Proceed,
                }
            });
        }
        window.add_controller(keys);

        CloseRequest::install(&window, &tw);

        Tabs::new(&tw).overview();
        // Restore the saved session (tabs + splits + per-pane history) if this workspace has one; else open a
        // single fresh shell. The debug hooks below still layer on top.
        match Session::open(&tw.ws.storage_dir(&Home::current().root())) {
            Ok(saved) if !saved.tabs.is_empty() => WindowSession::new(&tw).restore(&saved),
            Ok(_) => Tabs::new(&tw).terminal(),
            Err(error) => {
                hl_log::hl_error!(
                    hl_log::tag::RUNTIME,
                    "failed to restore terminal session for {}: {error}",
                    tw.ws.name
                );
                Tabs::new(&tw).terminal();
                if let Some(terminal) = tw.stack.visible_child().and_then(|page| TerminalPane::first(&page)) {
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
            if let Some(t) = tw.stack.visible_child().and_then(|c| TerminalPane::first(&c)) {
                *tw.focused.borrow_mut() = Some(t.clone());
                let o = if dir == "v" {
                    gtk::Orientation::Vertical
                } else {
                    gtk::Orientation::Horizontal
                };
                TerminalPane::new(&tw, &t).split(o);
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

    pub(crate) fn setup_hyperlinks(&self) {
        let term = self.0;
        let flags = PCRE2_UTF | PCRE2_NO_UTF_CHECK | PCRE2_MULTILINE | PCRE2_UCP | PCRE2_CASELESS;
        if let Ok(re) = vte4::Regex::for_match(URL_REGEX, flags) {
            let tag = term.match_add_regex(&re, 0);
            term.match_set_cursor_name(tag, "pointer");
        }
        term.set_mouse_autohide(true);

        // Hover cue: reflect the hovered OSC-8 link in the tooltip.
        term.connect_hyperlink_hover_uri_notify(|t| {
            let uri = t.hyperlink_hover_uri();
            t.set_tooltip_text(uri.as_deref());
        });

        // Click-to-open. Primary click over a URL match opens it; a modifier (Cmd/Ctrl) always opens the link
        // under the pointer even for explicit OSC-8 links.
        let click = gtk::GestureClick::new();
        click.set_button(1); // primary only
        let t = term.clone();
        click.connect_released(move |g, _n, x, y| {
            // Cmd/Ctrl-click opens the link under the pointer (an explicit OSC-8 hyperlink, else a regex URL
            // match). A modifier is required so a plain click / text selection is never hijacked.
            let state = g.current_event_state();
            let modified =
                state.contains(gdk::ModifierType::META_MASK) || state.contains(gdk::ModifierType::CONTROL_MASK);
            if !modified {
                return;
            }
            let uri = t.hyperlink_hover_uri().or_else(|| t.check_match_at(x, y).0);
            if let Some(uri) = uri {
                Url::new(&uri).open();
            }
        });
        term.add_controller(click);
    }

    /// The terminal's current directory, decoded from OSC 7's `file://` URI.
    pub(crate) fn working_directory(&self) -> Option<String> {
        let uri = self.0.current_directory_uri()?;
        session::WorkingDirectory::from_osc7(&uri).map(|path| path.into_string())
    }

    /// Extract the whole scrollback and visible screen as plain text.
    pub(crate) fn history(&self) -> String {
        let term = self.0;
        // The vadjustment spans the whole buffer: value range [lower, upper); rows are 1:1 with it.
        let (first, last) = match term.vadjustment() {
            Some(adj) => (adj.lower() as i64, adj.upper() as i64),
            None => (0, term.row_count()),
        };
        let (text, _len) = term.text_range_format(vte4::Format::Text, first, 0, last, -1);
        let raw = text.map(|g| g.to_string()).unwrap_or_default();
        // Cap the persisted history so a huge scrollback doesn't bloat the session on disk.
        session::History::new(&raw).clamp(5000)
    }
}

/// A normalized URL opened by the desktop's registered handler.
pub(crate) struct Url(String);

impl Url {
    pub(crate) fn new(url: &str) -> Self {
        Self(if url.starts_with("www.") {
            format!("https://{url}")
        } else {
            url.to_string()
        })
    }

    pub(crate) fn open(&self) {
        let _ = crate::gtk_adapter::Uri::new(&self.0).open();
    }
}

use crate::*;

mod close;
mod launch;
mod pane;
mod search;
mod slots;
mod state;

pub(crate) use close::*;
pub(crate) use launch::*;
pub(crate) use pane::*;
pub(crate) use slots::*;
pub(crate) use state::*;

#[cfg(test)]
mod shortcut_tests {
    use super::Shortcut;
    use gtk::gdk;

    #[test]
    fn macos_edit_shortcuts_never_fall_through_to_vte() {
        let command = gdk::ModifierType::META_MASK;
        assert_eq!(Shortcut::from_key(gdk::Key::c, command), Some(Shortcut::Copy));
        assert_eq!(Shortcut::from_key(gdk::Key::x, command), Some(Shortcut::Cut));
        assert_eq!(Shortcut::from_key(gdk::Key::v, command), Some(Shortcut::Paste));
        assert_eq!(Shortcut::from_key(gdk::Key::a, command), Some(Shortcut::SelectAll));
        assert_eq!(Shortcut::from_key(gdk::Key::c, gdk::ModifierType::empty()), None);
    }
}
