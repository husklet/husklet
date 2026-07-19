use super::*;

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
                    .and_then(|f| std::fs::read_to_string(Session::history_path(storage, f)).ok());
                // Reuse the pane's saved slot (fresh one if the session predates slots), and restore ONLY if
                // that slot actually has a frozen checkpoint on disk.
                let slot = Slots::new(tw).adopt(&pane.slot);
                let restore = Slots::has_checkpoint(&tw.ws, &slot);
                let (term, pid) = make_terminal_ex(tw, pane.cwd.clone(), history, slot, restore);
                pids.push(pid);
                (term.clone().upcast(), Some(term))
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
        let dash = Overview::new(&tw.ws, tw.overview_page).view();
        self.add(&tw.ws.name, Some("◧"), &dash, false);
    }

    pub(crate) fn terminal(&self) {
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
        let (term, pid) = make_terminal_ex(tw, cwd, None, Slots::new(tw).allocate(), false);
        paneroot.append(&term);
        let name = self.add(&format!("shell {n}"), None, &paneroot, true);
        tw.pids.borrow_mut().entry(name).or_default().push(pid);
        term.grab_focus();
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
            ProcessGroup::new(p.get()).hangup();
        }
        if let Some(child) = tw.stack.child_by_name(name) {
            // A user-closed tab must forget its panes' frozen checkpoints so they don't restore next time.
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

/// Find the first VTE terminal in `w`'s subtree (used by the HL_TERM_SPLIT screenshot hook).
pub(crate) struct TerminalPane<'a> {
    window: &'a Rc<TermWin>,
    terminal: vte4::Terminal,
}

impl<'a> TerminalPane<'a> {
    pub(crate) fn new(window: &'a Rc<TermWin>, terminal: &vte4::Terminal) -> Self {
        Self {
            window,
            terminal: terminal.clone(),
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
        let Some(parent) = old.parent() else { return };
        let page = Page::of(tw, old.upcast_ref::<gtk::Widget>()).map(|page| page.name);
        // OSC-7: split panes inherit the source pane's cwd. A fresh split gets a fresh slot; never restores.
        let split_cwd = old
            .current_directory_uri()
            .and_then(|u| session::WorkingDirectory::from_osc7(&u).map(|path| path.into_string()));
        let (new, pid) = make_terminal_ex(tw, split_cwd, None, Slots::new(tw).allocate(), false);
        if let Some(name) = &page {
            tw.pids
                .borrow_mut()
                .entry(name.clone())
                .or_default()
                .push(pid);
        }
        let paned = gtk::Paned::new(orient);
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_hexpand(true);
        paned.set_vexpand(true);

        if let Some(bx) = parent.downcast_ref::<gtk::Box>() {
            bx.remove(&old);
            paned.set_start_child(Some(&old));
            paned.set_end_child(Some(&new));
            bx.append(&paned);
        } else if let Some(pp) = parent.downcast_ref::<gtk::Paned>() {
            let is_start = pp.start_child().as_ref() == Some(old.upcast_ref::<gtk::Widget>());
            if is_start {
                pp.set_start_child(gtk::Widget::NONE);
            } else {
                pp.set_end_child(gtk::Widget::NONE);
            }
            paned.set_start_child(Some(&old));
            paned.set_end_child(Some(&new));
            if is_start {
                pp.set_start_child(Some(&paned));
            } else {
                pp.set_end_child(Some(&paned));
            }
        } else {
            return;
        }
        new.grab_focus();
    }

    /// A shell exited → close its pane. If it's in a split, collapse the split (keep the sibling);
    /// otherwise close the whole tab.
    pub(crate) fn close(&self) {
        let tw = self.window;
        let term = &self.terminal;
        // During a window close we deliberately kill the shells AFTER freezing them — that kill fires this
        // handler, but the freeze must survive, so do nothing here.
        if tw.closing.get() {
            return;
        }
        // The shell exited (or a split is collapsing) → this pane is gone for good; drop its slot + any stale
        // frozen checkpoint so a reopen won't resurrect it. (If this closes the whole tab, close_page handles
        // the rest; this terminal is already out of the registry so there's no double work.)
        Slots::new(tw).discard(term);
        let Some(parent) = term.parent() else { return };
        if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
            let is_start = paned.start_child().as_ref() == Some(term.upcast_ref::<gtk::Widget>());
            let sibling = if is_start {
                paned.end_child()
            } else {
                paned.start_child()
            };
            let Some(sibling) = sibling else {
                if let Some(page) = Page::of(tw, term.upcast_ref()) {
                    page.close();
                }
                return;
            };
            paned.set_start_child(gtk::Widget::NONE);
            paned.set_end_child(gtk::Widget::NONE);
            let Some(pparent) = paned.parent() else {
                return;
            };
            PaneReplacement::replace(&pparent, paned, &sibling);
            sibling.grab_focus();
        } else if let Some(page) = Page::of(tw, term.upcast_ref()) {
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
