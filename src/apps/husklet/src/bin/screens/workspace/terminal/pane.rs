use super::*;

fn page_owns_focus<T: PartialEq>(focused: Option<&T>, page: &[T]) -> bool {
    focused.is_some_and(|focused| page.iter().any(|candidate| candidate == focused))
}

pub(crate) struct PaneWidget;

impl PaneWidget {
    pub(crate) fn build(
        session: &WindowSession<'_>,
        node: &PaneNode,
        storage: &std::path::Path,
        pids: &mut Vec<Rc<Cell<i32>>>,
    ) -> (gtk::Widget, Option<vte4::Terminal>) {
        let tw = session.window;
        match node {
            PaneNode::Leaf(pane) => {
                let history = pane
                    .history_file
                    .as_ref()
                    .and_then(|file| match HistorySnapshot::read(storage, file) {
                        Ok(history) => Some(history),
                        Err(error) => {
                            hl_log::hl_error!(
                                hl_log::tag::RUNTIME,
                                "failed to restore terminal history workspace={:?} file={file:?} error={error}",
                                session.window.ws.name
                            );
                            None
                        }
                    });
                // Reuse the pane's saved layout slot (fresh one if the session predates slots).
                let slot = Slots::new(tw).adopt(pane.slot.as_deref());
                let (term, pid) = make_terminal_ex(tw, pane.cwd.clone(), history, &slot);
                pids.push(pid);
                (term.clone().upcast(), Some(term))
            }
            PaneNode::Surface(pane) => {
                // Reuse the pane's saved slot so an extension addressing its own
                // pane still finds it after a restart.
                let slot = Slots::new(tw).adopt(pane.slot.as_deref());
                (Surface::build(tw, &pane.extension, slot), None)
            }
            PaneNode::Split { dir, ratio, a, b } => {
                let orient = if *dir == SplitDir::Horizontal {
                    gtk::Orientation::Horizontal
                } else {
                    gtk::Orientation::Vertical
                };
                let paned = gtk::Paned::new(orient);
                paned.set_resize_start_child(true);
                paned.set_resize_end_child(true);
                paned.set_hexpand(true);
                paned.set_vexpand(true);
                let (wa, fa) = session.build_pane_widget(a, storage, pids);
                let (wb, fb) = session.build_pane_widget(b, storage, pids);
                paned.set_start_child(Some(&wa));
                paned.set_end_child(Some(&wb));
                // Apply the saved split ratio once the paned has been allocated a size.
                SplitPosition::restore(&paned, *dir, *ratio);
                (paned.upcast(), fa.or(fb))
            }
        }
    }
}

pub(crate) struct SplitPosition;

impl SplitPosition {
    pub(crate) fn restore(paned: &gtk::Paned, direction: SplitDir, ratio: f64) {
        let paned = paned.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            let dimension = match direction {
                SplitDir::Horizontal => paned.width(),
                SplitDir::Vertical => paned.height(),
            };
            if dimension > 1 {
                paned.set_position((ratio * dimension as f64).round() as i32);
            }
        });
    }
}

pub(crate) struct Tabs<'a> {
    window: &'a Rc<TermWin>,
}

impl<'a> Tabs<'a> {
    pub(crate) fn new(window: &'a Rc<TermWin>) -> Self {
        Self { window }
    }

    pub(crate) fn add(
        &self,
        title: &str,
        icon: Option<&str>,
        content: &impl IsA<gtk::Widget>,
        closable: bool,
    ) -> String {
        let tw = self.window;
        let id = tw.counter.get();
        tw.counter.set(id + 1);
        let name = format!("p{id}");
        tw.stack.add_named(content, Some(&name));

        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bx.add_css_class("tab");
        bx.set_hexpand(true);
        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        inner.set_hexpand(true);
        inner.set_halign(gtk::Align::Center);
        if let Some(ic) = icon {
            let il = gtk::Label::new(Some(ic));
            il.add_css_class("di");
            inner.append(&il);
        }
        let lbl = gtk::Label::new(Some(title));
        lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        inner.append(&lbl);
        bx.append(&inner);
        if closable {
            let x = gtk::Button::from_icon_name("window-close-symbolic");
            x.add_css_class("tabx");
            let tw2 = tw.clone();
            let name2 = name.clone();
            x.connect_clicked(move |_| Page::new(&tw2, &name2).close());
            bx.append(&x);
        }
        let click = gtk::GestureClick::new();
        let tw2 = tw.clone();
        let name2 = name.clone();
        click.connect_released(move |_, _, _, _| Page::new(&tw2, &name2).select());
        bx.add_controller(click);

        tw.tabs.append(&bx);
        tw.entries.borrow_mut().push(TabEntry {
            name: name.clone(),
            button: bx,
        });
        Page::new(tw, &name).select();
        name
    }

    pub(crate) fn overview(&self) {
        let tw = self.window;
        let dash = Overview::new(&tw.ws, tw.overview_page).within(tw).view();
        self.add(&tw.ws.name, Some("◧"), &dash, false);
    }

    /// Opens a shell tab and hands back its identity.
    ///
    /// The identity is returned rather than dropped because an extension that
    /// asked for a tab has to be told which one it got.
    pub(crate) fn terminal(&self) -> String {
        let tw = self.window;
        let n = tw.shell_no.get() + 1;
        tw.shell_no.set(n);
        let paneroot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        paneroot.set_hexpand(true);
        paneroot.set_vexpand(true);
        // OSC-7: open the new tab in the currently-focused shell's cwd. A brand-new tab gets a fresh slot
        // and never restores (nothing frozen for it yet).
        let cwd = tw
            .focused
            .borrow()
            .as_ref()
            .and_then(|terminal| Terminal::new(terminal).working_directory());
        let (term, pid) = make_terminal_ex(tw, cwd, None, &Slots::new(tw).allocate());
        paneroot.append(&term);
        let name = self.add(&format!("shell {n}"), None, &paneroot, true);
        tw.pids.borrow_mut().entry(name.clone()).or_default().push(pid);
        term.grab_focus();
        name
    }
}

pub(crate) struct CurrentPage;

impl CurrentPage {
    pub(crate) fn close(window: &Rc<TermWin>) {
        let Some(name) = window.stack.visible_child_name() else {
            return;
        };
        Page::new(window, name.as_str()).close();
    }
}

pub(crate) struct Page<'a> {
    window: &'a Rc<TermWin>,
    name: String,
}

impl<'a> Page<'a> {
    pub(crate) fn new(window: &'a Rc<TermWin>, name: &str) -> Self {
        Self {
            window,
            name: name.to_owned(),
        }
    }

    pub(crate) fn select(&self) {
        let tw = self.window;
        let name = self.name.as_str();
        if !tw.entries.borrow().iter().any(|e| e.name == name) {
            return;
        }
        tw.stack.set_visible_child_name(name);
        for e in tw.entries.borrow().iter() {
            if e.name == name {
                e.button.add_css_class("on");
            } else {
                e.button.remove_css_class("on");
            }
        }
    }

    pub(crate) fn close(&self) {
        let tw = self.window;
        let name = self.name.as_str();
        // Non-closable overview (first tab) stays.
        if tw.entries.borrow().first().map(|e| e.name.as_str()) == Some(name) {
            return;
        }
        for p in tw.pids.borrow_mut().remove(name).unwrap_or_default() {
            if let Err(error) = ProcessGroup::new(p.get()).hangup() {
                hl_log::hl_warn!(hl_log::tag::RUNTIME, "terminal process hangup ignored: {error}");
            }
        }
        if let Some(child) = tw.stack.child_by_name(name) {
            let mut terminals = Vec::new();
            PaneView::collect(&child, &mut terminals);
            let clear_focus = {
                let focused = tw.focused.borrow();
                page_owns_focus(focused.as_ref(), &terminals)
            };
            if clear_focus {
                tw.focused.borrow_mut().take();
            }
            // A user-closed tab must forget its pane registry entries.
            Slots::new(tw).discard_page(&child);
            tw.stack.remove(&child);
        }
        let mut pos = None;
        {
            let mut es = tw.entries.borrow_mut();
            if let Some(i) = es.iter().position(|e| e.name == name) {
                tw.tabs.remove(&es[i].button);
                es.remove(i);
                pos = Some(i.min(es.len().saturating_sub(1)));
            }
        }
        if let Some(i) = pos {
            let next = tw.entries.borrow().get(i).map(|e| e.name.clone());
            if let Some(n) = next {
                Self::new(tw, &n).select();
            }
        }
    }

    /// Walk up from `widget` to the page owned by the window's stack.
    pub(crate) fn of(window: &'a Rc<TermWin>, widget: &gtk::Widget) -> Option<Self> {
        let mut current = widget.clone();
        loop {
            let parent = current.parent()?;
            if parent.downcast_ref::<gtk::Stack>().is_some() {
                let name = window.stack.page(&current).name()?.to_string();
                return Some(Self { window, name });
            }
            current = parent;
        }
    }
}

/// Find the first VTE terminal in `w`'s subtree (used by the `HL_TERM_SPLIT` screenshot hook).
pub(crate) struct PaneView<'a> {
    window: &'a Rc<TermWin>,
    terminal: vte4::Terminal,
}

impl<'a> PaneView<'a> {
    pub(crate) fn new(window: &'a Rc<TermWin>, terminal: &vte4::Terminal) -> Self {
        Self {
            window,
            terminal: terminal.clone(),
        }
    }

    /// Every VTE terminal in `w`'s subtree, in layout order.
    ///
    /// `first` answers the pane a split acts on; a headless verification run has
    /// to read them all, because a window with two panes and one live shell is
    /// exactly the failure worth catching.
    pub(crate) fn all(w: &gtk::Widget, found: &mut Vec<vte4::Terminal>) {
        if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
            found.push(t.clone());
            return;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            Self::all(&c, found);
            child = c.next_sibling();
        }
    }

    pub(crate) fn first(w: &gtk::Widget) -> Option<vte4::Terminal> {
        if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
            return Some(t.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(t) = Self::first(&c) {
                return Some(t);
            }
            child = c.next_sibling();
        }
        None
    }

    pub(crate) fn collect(widget: &gtk::Widget, terminals: &mut Vec<vte4::Terminal>) {
        if let Some(terminal) = widget.downcast_ref::<vte4::Terminal>() {
            terminals.push(terminal.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            Self::collect(&current, terminals);
            child = current.next_sibling();
        }
    }

    pub(crate) fn split(&self, orient: gtk::Orientation) {
        let tw = self.window;
        let old = self.terminal.clone();
        if old.parent().is_none() {
            return;
        }
        let page = Page::of(tw, old.upcast_ref::<gtk::Widget>()).map(|page| page.name);
        // OSC-7: split panes inherit the source pane's cwd. A fresh split gets a fresh slot; never restores.
        let split_cwd = old
            .current_directory_uri()
            .and_then(|u| session::WorkingDirectory::from_osc7(&u).map(hl_ws_term::WorkingDirectory::into_string));
        let (new, pid) = make_terminal_ex(tw, split_cwd, None, &Slots::new(tw).allocate());
        if let Some(name) = &page {
            tw.pids.borrow_mut().entry(name.clone()).or_default().push(pid);
        }
        if PaneSplit::insert(old.upcast_ref::<gtk::Widget>(), orient, new.upcast_ref::<gtk::Widget>()) {
            new.grab_focus();
        }
    }

    /// A shell exited → close its pane. If it's in a split, collapse the split (keep the sibling);
    /// otherwise close the whole tab.
    pub(crate) fn close(&self) {
        let tw = self.window;
        // Window teardown owns registry cleanup while it terminates worker processes.
        if tw.closing.get() {
            return;
        }
        // The shell exited (or a split is collapsing), so this pane is gone from the live registry.
        Slots::new(tw).discard(&self.terminal);
        PaneClosure::remove(tw, self.terminal.upcast_ref::<gtk::Widget>());
    }
}

/// Putting a new pane beside an existing one.
pub(crate) struct PaneSplit;

impl PaneSplit {
    /// Divides `old` in place, with `new` in the half that appeared.
    ///
    /// Takes widgets rather than terminals because the half a pane is divided
    /// into may hold an extension's interface instead of a shell, and the shape
    /// of the split is the same either way. Answers whether it happened: a pane
    /// whose parent is neither a box nor a split is not laid out by this window.
    pub(crate) fn insert(old: &gtk::Widget, orientation: gtk::Orientation, new: &gtk::Widget) -> bool {
        let Some(parent) = old.parent() else { return false };
        let paned = gtk::Paned::new(orientation);
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_hexpand(true);
        paned.set_vexpand(true);
        if let Some(container) = parent.downcast_ref::<gtk::Box>() {
            container.remove(old);
            Self::fill(&paned, old, new);
            container.append(&paned);
            return true;
        }
        let Some(outer) = parent.downcast_ref::<gtk::Paned>() else {
            return false;
        };
        Self::nest(outer, &paned, old, new);
        true
    }

    /// The two halves of a fresh split.
    fn fill(paned: &gtk::Paned, old: &gtk::Widget, new: &gtk::Widget) {
        paned.set_start_child(Some(old));
        paned.set_end_child(Some(new));
    }

    /// Puts a fresh split where `old` sat inside the split that already held it.
    fn nest(outer: &gtk::Paned, paned: &gtk::Paned, old: &gtk::Widget, new: &gtk::Widget) {
        let is_start = outer.start_child().as_ref() == Some(old);
        if is_start {
            outer.set_start_child(gtk::Widget::NONE);
        } else {
            outer.set_end_child(gtk::Widget::NONE);
        }
        Self::fill(paned, old, new);
        if is_start {
            outer.set_start_child(Some(paned));
        } else {
            outer.set_end_child(Some(paned));
        }
    }
}

/// Taking one pane out of the layout, whatever it held.
pub(crate) struct PaneClosure;

impl PaneClosure {
    /// Removes one pane: collapse its split onto the sibling, or close the tab
    /// when it was the tab's only pane. Registry cleanup is the caller's,
    /// because a terminal and a surface are forgotten from different places.
    pub(crate) fn remove(window: &Rc<TermWin>, pane: &gtk::Widget) {
        let Some(parent) = pane.parent() else { return };
        let Some(paned) = parent.downcast_ref::<gtk::Paned>() else {
            Self::page(window, pane);
            return;
        };
        let is_start = paned.start_child().as_ref() == Some(pane);
        let sibling = if is_start {
            paned.end_child()
        } else {
            paned.start_child()
        };
        let Some(sibling) = sibling else {
            Self::page(window, pane);
            return;
        };
        paned.set_start_child(gtk::Widget::NONE);
        paned.set_end_child(gtk::Widget::NONE);
        let Some(outer) = paned.parent() else { return };
        PaneReplacement::replace(&outer, paned, &sibling);
        sibling.grab_focus();
    }

    /// Closing the last pane of a tab is closing the tab, which is what closing
    /// that pane by hand already does.
    fn page(window: &Rc<TermWin>, pane: &gtk::Widget) {
        if let Some(page) = Page::of(window, pane) {
            page.close();
        }
    }
}

pub(crate) struct PaneReplacement;

impl PaneReplacement {
    pub(crate) fn replace(parent: &gtk::Widget, pane: &gtk::Paned, sibling: &gtk::Widget) {
        if let Some(container) = parent.downcast_ref::<gtk::Box>() {
            container.remove(pane);
            container.append(sibling);
            return;
        }
        let Some(parent_pane) = parent.downcast_ref::<gtk::Paned>() else {
            return;
        };
        if parent_pane.start_child().as_ref() == Some(pane.upcast_ref::<gtk::Widget>()) {
            parent_pane.set_start_child(Some(sibling));
        } else {
            parent_pane.set_end_child(Some(sibling));
        }
    }
}

#[cfg(test)]
mod focus_ownership_tests {
    use super::page_owns_focus;

    #[test]
    fn removed_page_clears_only_focus_owned_by_that_page() {
        assert!(page_owns_focus(Some(&2), &[1, 2, 3]));
        assert!(!page_owns_focus(Some(&4), &[1, 2, 3]));
        assert!(!page_owns_focus::<i32>(None, &[1, 2, 3]));
    }
}
