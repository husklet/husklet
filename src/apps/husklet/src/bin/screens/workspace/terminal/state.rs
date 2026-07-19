use super::*;

pub(crate) struct CopyMode(Cell<bool>);

impl CopyMode {
    pub(crate) fn new() -> Self {
        Self(Cell::new(false))
    }

    pub(crate) fn is_active(&self) -> bool {
        self.0.get()
    }

    pub(crate) fn enter(&self, terminal: Option<vte4::Terminal>) {
        self.0.set(true);
        if let Some(t) = terminal {
            t.add_css_class("copymode");
        }
    }

    pub(crate) fn exit(&self, terminal: Option<vte4::Terminal>) {
        self.0.set(false);
        if let Some(t) = terminal {
            t.remove_css_class("copymode");
        }
    }

    /// Handle a plain (unmodified/Ctrl) key while in copy mode. Returns true if the key was consumed
    /// (copy mode swallows all keys so nothing leaks into the shell).
    pub(crate) fn key(&self, tw: &Rc<TermWin>, key: gdk::Key, state: gdk::ModifierType) -> bool {
        let Some(t) = tw.focused.borrow().clone() else {
            self.exit(None);
            return true;
        };
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let scroll = |lines: f64| {
            if let Some(adj) = t.vadjustment() {
                let max = (adj.upper() - adj.page_size()).max(adj.lower());
                adj.set_value((adj.value() + lines).clamp(adj.lower(), max));
            }
        };
        let page = t.vadjustment().map(|a| a.page_size()).unwrap_or(20.0);
        match key {
            gdk::Key::Escape | gdk::Key::q | gdk::Key::Q => self.exit(Some(t.clone())),
            gdk::Key::j | gdk::Key::Down => scroll(1.0),
            gdk::Key::k | gdk::Key::Up => scroll(-1.0),
            gdk::Key::d | gdk::Key::D if ctrl => scroll(page / 2.0),
            gdk::Key::u | gdk::Key::U if ctrl => scroll(-page / 2.0),
            gdk::Key::Page_Down | gdk::Key::space => scroll(page),
            gdk::Key::Page_Up => scroll(-page),
            gdk::Key::g if !shift => {
                if let Some(adj) = t.vadjustment() {
                    adj.set_value(adj.lower());
                }
            }
            gdk::Key::g | gdk::Key::G if shift => {
                if let Some(adj) = t.vadjustment() {
                    adj.set_value((adj.upper() - adj.page_size()).max(adj.lower()));
                }
            }
            gdk::Key::slash => {
                self.exit(Some(t.clone()));
                tw.search.toggle(Some(t.clone()));
            }
            gdk::Key::a | gdk::Key::A | gdk::Key::v | gdk::Key::V => t.select_all(),
            gdk::Key::y | gdk::Key::Y | gdk::Key::Return | gdk::Key::KP_Enter => {
                if t.has_selection() {
                    t.copy_clipboard_format(vte4::Format::Text);
                }
                self.exit(Some(t.clone()));
            }
            _ => {}
        }
        true
    }
}

// -------------------------------------------------------------------------------------------------
// Session / multiplexer — persist the tab+split layout + per-pane history; restore on reopen.
// -------------------------------------------------------------------------------------------------

/// Snapshot the window's tabs (skipping the overview) + each pane's scrollback into a [`Session`] and
/// write it (layout + history files) under the workspace storage dir.
pub(crate) struct WindowSession<'a> {
    pub(crate) window: &'a Rc<TermWin>,
}

impl<'a> WindowSession<'a> {
    pub(crate) fn new(window: &'a Rc<TermWin>) -> Self {
        Self { window }
    }

    pub(crate) fn save(&self) {
        let tw = self.window;
        let storage = tw.ws.storage_dir(&Home::current().root());
        // Fresh history files each save (avoid stale accumulation).
        let dir = Session::dir(&storage);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let mut hist_idx = 0usize;
        let mut tabs = Vec::new();
        // entries[0] is the non-closable overview; shells are the rest.
        let entries: Vec<(String, String)> = {
            let es = tw.entries.borrow();
            es.iter()
                .skip(1)
                .map(|e| (e.name.clone(), e.title()))
                .collect()
        };
        for (page_name, title) in entries {
            let Some(child) = tw.stack.child_by_name(&page_name) else {
                continue;
            };
            if let Some(root) = self.snapshot_node(&child, &storage, &mut hist_idx) {
                tabs.push(SessionTab { title, root });
            }
        }
        let session = Session { tabs };
        if session.tabs.is_empty() {
            Session::clear(&storage);
        } else {
            let _ = session.save(&storage);
        }
    }

    pub(crate) fn snapshot_node(
        &self,
        widget: &gtk::Widget,
        storage: &std::path::Path,
        history_index: &mut usize,
    ) -> Option<PaneNode> {
        PaneSnapshot::capture(self, widget, storage, history_index)
    }
}

impl TabEntry {
    /// Read the tab button's title label text for restoring the tab title.
    pub(crate) fn title(&self) -> String {
        // button = [inner Box [ (icon?) label ], (x button)]; find the first Label in the tree.
        Self::find_label(self.button.upcast_ref()).unwrap_or_else(|| "shell".to_string())
    }

    pub(crate) fn find_label(w: &gtk::Widget) -> Option<String> {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            let t = l.text().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            if let Some(s) = Self::find_label(&ch) {
                return Some(s);
            }
            c = ch.next_sibling();
        }
        None
    }
}

/// Walk a page's widget subtree into a [`PaneNode`], dumping each terminal's history to a file and
/// recording each pane's stable layout slot.
pub(crate) struct PaneSnapshot;

impl PaneSnapshot {
    pub(crate) fn capture(
        session: &WindowSession<'_>,
        w: &gtk::Widget,
        storage: &std::path::Path,
        hist_idx: &mut usize,
    ) -> Option<PaneNode> {
        let tw = session.window;
        if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
            let cwd = PaneWorkingDirectory::read(t);
            let text = Terminal::new(t).history();
            let history_file = HistorySnapshot::write(storage, &text, hist_idx);
            let slot = Slots::new(tw).of(t);
            return Some(PaneNode::Leaf(Pane {
                cwd,
                history_file,
                slot,
            }));
        }
        if let Some(paned) = w.downcast_ref::<gtk::Paned>() {
            let dir = if paned.orientation() == gtk::Orientation::Horizontal {
                SplitDir::Horizontal
            } else {
                SplitDir::Vertical
            };
            let dim = if dir == SplitDir::Horizontal {
                paned.width()
            } else {
                paned.height()
            };
            let ratio = if dim > 1 {
                paned.position() as f64 / dim as f64
            } else {
                0.5
            };
            let a = paned
                .start_child()
                .and_then(|c| session.snapshot_node(&c, storage, hist_idx));
            let b = paned
                .end_child()
                .and_then(|c| session.snapshot_node(&c, storage, hist_idx));
            return match (a, b) {
                (Some(a), Some(b)) => Some(PaneNode::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            };
        }
        // A container (the paneroot Box) — descend into its first meaningful child.
        let mut c = w.first_child();
        while let Some(ch) = c {
            if let Some(n) = session.snapshot_node(&ch, storage, hist_idx) {
                return Some(n);
            }
            c = ch.next_sibling();
        }
        None
    }
}

pub(crate) struct HistorySnapshot;

pub(crate) struct PaneWorkingDirectory;

impl PaneWorkingDirectory {
    pub(crate) fn read(terminal: &vte4::Terminal) -> Option<String> {
        let uri = terminal.current_directory_uri()?;
        session::WorkingDirectory::from_osc7(&uri).map(|path| path.into_string())
    }
}

impl HistorySnapshot {
    pub(crate) fn write(
        storage: &std::path::Path,
        text: &str,
        index: &mut usize,
    ) -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }
        let file = format!("hist-{}.txt", *index);
        *index += 1;
        std::fs::write(Session::history_path(storage, &file), text)
            .is_ok()
            .then_some(file)
    }
}

/// Rebuild all tabs from a saved [`Session`].
impl WindowSession<'_> {
    pub(crate) fn restore(&self, session: &Session) {
        let tw = self.window;
        let storage = tw.ws.storage_dir(&Home::current().root());
        for tab in &session.tabs {
            let n = tw.shell_no.get() + 1;
            tw.shell_no.set(n);
            let paneroot = gtk::Box::new(gtk::Orientation::Vertical, 0);
            paneroot.set_hexpand(true);
            paneroot.set_vexpand(true);
            let mut pids = Vec::new();
            let (widget, first) = self.build_pane_widget(&tab.root, &storage, &mut pids);
            paneroot.append(&widget);
            let title = if tab.title.is_empty() {
                format!("shell {n}")
            } else {
                tab.title.clone()
            };
            let name = Tabs::new(tw).add(&title, None, &paneroot, true);
            tw.pids.borrow_mut().entry(name).or_default().extend(pids);
            if let Some(t) = first {
                t.grab_focus();
            }
        }
    }

    pub(crate) fn build_pane_widget(
        &self,
        node: &PaneNode,
        storage: &std::path::Path,
        pids: &mut Vec<Rc<Cell<i32>>>,
    ) -> (gtk::Widget, Option<vte4::Terminal>) {
        PaneWidget::build(self, node, storage, pids)
    }
}
