use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static SESSION_GENERATION: AtomicU64 = AtomicU64::new(0);

pub(crate) struct CopyMode(Cell<bool>);

#[derive(Debug, Eq, PartialEq)]
struct CopyModeFocusTransition {
    clear_previous: bool,
    mark_current: bool,
}

impl CopyModeFocusTransition {
    fn new(active: bool, has_previous: bool, previous_is_current: bool) -> Self {
        Self {
            clear_previous: active && has_previous && !previous_is_current,
            mark_current: active,
        }
    }
}

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

    /// Keep the visual owner of an active window-global copy mode aligned with keyboard focus.
    pub(crate) fn focus(&self, previous: Option<vte4::Terminal>, current: &vte4::Terminal) {
        let transition = CopyModeFocusTransition::new(
            self.is_active(),
            previous.is_some(),
            previous.as_ref().is_some_and(|terminal| terminal == current),
        );
        if transition.clear_previous {
            if let Some(previous) = previous {
                previous.remove_css_class("copymode");
            }
        }
        if transition.mark_current {
            current.add_css_class("copymode");
        }
    }

    /// Handle a plain (unmodified/Ctrl) key while in copy mode. Returns true if the key was consumed
    /// (copy mode swallows all keys so nothing leaks into the shell).
    pub(crate) fn key(&self, tw: &Rc<TermWin>, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if is_window_shortcut(state) {
            return false;
        }
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
        let page = t.vadjustment().map_or(20.0, |a| a.page_size());
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

#[cfg(test)]
mod copy_mode_tests {
    use super::CopyModeFocusTransition;

    #[test]
    fn active_copy_mode_follows_focus_between_panes() {
        assert_eq!(
            CopyModeFocusTransition::new(true, true, false),
            CopyModeFocusTransition {
                clear_previous: true,
                mark_current: true,
            }
        );
        assert_eq!(
            CopyModeFocusTransition::new(true, false, false),
            CopyModeFocusTransition {
                clear_previous: false,
                mark_current: true,
            }
        );
    }

    #[test]
    fn copy_mode_focus_is_stable_for_same_pane_and_inert_after_exit() {
        assert_eq!(
            CopyModeFocusTransition::new(true, true, true),
            CopyModeFocusTransition {
                clear_previous: false,
                mark_current: true,
            }
        );
        for (has_previous, previous_is_current) in [(false, false), (true, false), (true, true)] {
            assert_eq!(
                CopyModeFocusTransition::new(false, has_previous, previous_is_current),
                CopyModeFocusTransition {
                    clear_previous: false,
                    mark_current: false,
                }
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn is_window_shortcut(state: gdk::ModifierType) -> bool {
    state.contains(gdk::ModifierType::META_MASK)
}

#[cfg(not(target_os = "macos"))]
fn is_window_shortcut(state: gdk::ModifierType) -> bool {
    state.contains(gdk::ModifierType::CONTROL_MASK) && state.contains(gdk::ModifierType::SHIFT_MASK)
}

#[cfg(test)]
mod tests {
    use super::is_window_shortcut;
    use gtk::gdk;

    #[test]
    fn copy_mode_leaves_window_shortcuts_for_the_window_dispatcher() {
        #[cfg(target_os = "macos")]
        {
            assert!(is_window_shortcut(gdk::ModifierType::META_MASK));
            assert!(!is_window_shortcut(gdk::ModifierType::CONTROL_MASK));
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(is_window_shortcut(
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK
            ));
            assert!(!is_window_shortcut(gdk::ModifierType::CONTROL_MASK));
            assert!(!is_window_shortcut(gdk::ModifierType::SHIFT_MASK));
        }
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

    pub(crate) fn save(&self) -> std::io::Result<()> {
        let tw = self.window;
        let storage = tw.ws.storage_dir(&Home::current().root());
        std::fs::create_dir_all(Session::dir(&storage))?;

        let mut hist_idx = 0usize;
        let generation = HistoryGeneration::new(&storage)?;
        let mut tabs = Vec::new();
        // entries[0] is the non-closable overview; shells are the rest.
        let entries: Vec<(String, String, bool)> = {
            let es = tw.entries.borrow();
            es.iter()
                .skip(1)
                .filter(|entry| entry.persisted)
                .map(|e| (e.name.clone(), e.title(), e.pinned))
                .collect()
        };
        for (page_name, title, pinned) in entries {
            let Some(child) = tw.stack.child_by_name(&page_name) else {
                continue;
            };
            if let Some(root) = self.snapshot_node(&child, &storage, generation.as_str(), &mut hist_idx)? {
                tabs.push(SessionTab { title, pinned, root });
            }
        }
        let session = Session { tabs };
        if session.tabs.is_empty() {
            Session::clear(&storage)
        } else {
            session.save(&storage)
        }
    }

    pub(crate) fn snapshot_node(
        &self,
        widget: &gtk::Widget,
        storage: &std::path::Path,
        generation: &str,
        history_index: &mut usize,
    ) -> std::io::Result<Option<PaneNode>> {
        PaneSnapshot::capture(self, widget, storage, generation, history_index)
    }
}

impl TabEntry {
    /// Read the tab button's title label text for restoring the tab title.
    pub(crate) fn title(&self) -> String {
        self.title.text().to_string()
    }

    pub(crate) fn retitle(&mut self, title: &str) {
        self.title.set_text(title);
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
        generation: &str,
        hist_idx: &mut usize,
    ) -> std::io::Result<Option<PaneNode>> {
        let tw = session.window;
        // A surface is a leaf before it is a container: the widgets under it are
        // the extension's drawing, and none of them is a pane of this window.
        if let Some((slot, extension, provider)) = Slots::new(tw).surface(w) {
            return Ok(Some(PaneNode::Surface(SurfacePane {
                extension,
                provider,
                slot: Some(slot),
            })));
        }
        if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
            let cwd = PaneWorkingDirectory::read(t);
            let text = Terminal::new(t).history();
            let history_file = HistorySnapshot::write(storage, generation, &text, hist_idx)?;
            let slot = Slots::new(tw).of(t);
            return Ok(Some(PaneNode::Leaf(Pane {
                cwd,
                history_file,
                slot,
            })));
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
                .map(|child| session.snapshot_node(&child, storage, generation, hist_idx))
                .transpose()?
                .flatten();
            let b = paned
                .end_child()
                .map(|child| session.snapshot_node(&child, storage, generation, hist_idx))
                .transpose()?
                .flatten();
            return Ok(match (a, b) {
                (Some(a), Some(b)) => Some(PaneNode::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            });
        }
        // A container (the paneroot Box) — descend into its first meaningful child.
        let mut c = w.first_child();
        while let Some(ch) = c {
            if let Some(n) = session.snapshot_node(&ch, storage, generation, hist_idx)? {
                return Ok(Some(n));
            }
            c = ch.next_sibling();
        }
        Ok(None)
    }
}

pub(crate) struct HistorySnapshot;

struct HistoryGeneration(String);

impl HistoryGeneration {
    fn new(storage: &std::path::Path) -> std::io::Result<Self> {
        let directory = Session::dir(storage);
        loop {
            let sequence = SESSION_GENERATION.fetch_add(1, Ordering::Relaxed);
            let candidate = format!("{}-{sequence}", std::process::id());
            if Self::available(&directory, &candidate)? {
                return Ok(Self(candidate));
            }
        }
    }

    fn available(directory: &std::path::Path, candidate: &str) -> std::io::Result<bool> {
        let prefix = format!("hist-{candidate}-");
        for entry in std::fs::read_dir(directory)? {
            let name = entry?.file_name();
            if name.to_string_lossy().starts_with(&prefix) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) struct PaneWorkingDirectory;

impl PaneWorkingDirectory {
    pub(crate) fn read(terminal: &vte4::Terminal) -> Option<String> {
        let uri = terminal.current_directory_uri()?;
        session::WorkingDirectory::from_osc7(&uri).map(hl_ws_term::WorkingDirectory::into_string)
    }
}

impl HistorySnapshot {
    /// Lines that describe one session's startup rather than the work done in it. Persisting them
    /// would replay a past restore as though it had just happened, so they are dropped from the
    /// scrollback that is written to disk. The reopen notice is one of them: it is shown once, for
    /// the restore it describes.
    const TRANSIENT_MESSAGES: [&'static str; 6] = [
        hl::runtime::domain::RESTORE_NOTICE_PREFIX,
        "workspace session ended immediately (",
        "failed to start shell: ",
        "hl-engine fault: ",
        "bash: cannot set terminal process group ",
        "bash: no job control in this shell",
    ];

    pub(super) fn persistent(text: &str) -> String {
        text.lines()
            .filter(|line| !Self::TRANSIENT_MESSAGES.iter().any(|message| line.contains(message)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn read(storage: &std::path::Path, file: &str) -> std::io::Result<String> {
        std::fs::read_to_string(Session::history_path(storage, file)?)
    }

    pub(crate) fn write(
        storage: &std::path::Path,
        generation: &str,
        text: &str,
        index: &mut usize,
    ) -> std::io::Result<Option<String>> {
        let text = Self::persistent(text);
        if text.trim().is_empty() {
            return Ok(None);
        }
        let file = format!("hist-{generation}-{}.txt", *index);
        *index += 1;
        hl_fs::File::from(Session::history_path(storage, &file)?).replace(text)?;
        Ok(Some(file))
    }
}

/// Rebuild all tabs from a saved [`Session`].
impl WindowSession<'_> {
    pub(crate) fn restore(&self, session: &Session) {
        self.restore_with(session, &ProductionPaneLauncher);
    }

    pub(crate) fn restore_with<L: PaneLauncher>(&self, session: &Session, launcher: &L) {
        let tw = self.window;
        launcher.observe(RestoreEvent::Started);
        let storage = tw.ws.storage_dir(&Home::current().root());
        for tab in &session.tabs {
            let n = tw.shell_no.get() + 1;
            tw.shell_no.set(n);
            let paneroot = gtk::Box::new(gtk::Orientation::Vertical, 0);
            paneroot.set_hexpand(true);
            paneroot.set_vexpand(true);
            let mut pids = Vec::new();
            let (widget, _first) = self.build_pane_widget(&tab.root, &storage, &mut pids, launcher);
            paneroot.append(&widget);
            let title = if tab.title.is_empty() {
                format!("shell {n}")
            } else {
                tab.title.clone()
            };
            let name = Tabs::new(tw).add_persisted(&title, None, &paneroot, true, tab.pinned);
            tw.pids.borrow_mut().entry(name).or_default().extend(pids);
        }
        launcher.observe(RestoreEvent::Completed);
    }

    pub(crate) fn build_pane_widget<L: PaneLauncher>(
        &self,
        node: &PaneNode,
        storage: &std::path::Path,
        pids: &mut Vec<Rc<Cell<i32>>>,
        launcher: &L,
    ) -> (gtk::Widget, Option<vte4::Terminal>) {
        PaneWidget::build(self, node, storage, pids, launcher)
    }
}

#[cfg(test)]
mod history_snapshot_tests {
    use super::{HistoryGeneration, HistorySnapshot};

    #[test]
    fn transient_launch_failures_are_not_persisted() {
        let history = concat!(
            "prior output\n",
            "workspace session ended immediately (exit code 255)\n",
            "live output\n",
            "failed to start shell: resource unavailable\n",
            "hl-engine fault: status=-1, detail=0\n",
            "bash: cannot set terminal process group (42): Bad file descriptor\n",
            "bash: no job control in this shell\n",
            "prompt\n",
        );

        assert_eq!(
            HistorySnapshot::persistent(history),
            "prior output\nlive output\nprompt"
        );
    }

    #[test]
    fn the_reopen_notice_is_shown_once_and_never_persisted_as_scrollback() {
        let notice = hl::runtime::domain::RESTORE_NOTICE_PREFIX;
        let history = format!(
            "guest output\n\
             {notice}Session restored. The output above is preserved history from your last session.\n\
             {notice}1 program could not be resumed as a live process:\n\
             {notice}  \u{2022} execution 7f3a: exec 7f3a cannot be reattached after a whole-image restore\n\
             later guest output\n"
        );

        assert_eq!(
            HistorySnapshot::persistent(&history),
            "guest output\nlater guest output"
        );
    }

    #[test]
    fn persisted_history_prevents_generation_reuse() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(temporary.path().join("hist-42-0-0.txt"), "history").unwrap();

        assert!(!HistoryGeneration::available(temporary.path(), "42-0").unwrap());
        assert!(HistoryGeneration::available(temporary.path(), "42-1").unwrap());
    }

    #[test]
    fn history_reads_report_missing_and_unsafe_references() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = hl_ws_term::session::Session::dir(temporary.path());
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("hist-current.txt"), "saved output").unwrap();

        assert_eq!(
            HistorySnapshot::read(temporary.path(), "hist-current.txt").unwrap(),
            "saved output"
        );
        assert_eq!(
            HistorySnapshot::read(temporary.path(), "hist-missing.txt")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(
            HistorySnapshot::read(temporary.path(), "../outside")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
