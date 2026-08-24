use super::*;

fn page_owns_focus<T: PartialEq>(focused: Option<&T>, page: &[T]) -> bool {
    focused.is_some_and(|focused| page.iter().any(|candidate| candidate == focused))
}

pub(super) struct PaneFocus;

impl PaneFocus {
    pub(super) fn wire(tw: &Rc<TermWin>, terminal: &vte4::Terminal) {
        let tw = tw.clone();
        let focused = terminal.clone();
        let controller = gtk::EventControllerFocus::new();
        controller.connect_enter(move |_| {
            if let Some(page) = Page::of(&tw, focused.upcast_ref::<gtk::Widget>()) {
                tw.page_focus.borrow_mut().insert(page.name, focused.downgrade());
            }
            let previous = tw.focused.replace(Some(focused.clone()));
            tw.copymode.focus(previous.clone(), &focused);
            tw.search.focus(previous, focused.clone());
        });
        terminal.add_controller(controller);
    }
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
        paned.add_tick_callback(move |paned, _| {
            let dimension = match direction {
                SplitDir::Horizontal => paned.width(),
                SplitDir::Vertical => paned.height(),
            };
            if dimension <= 1 {
                return glib::ControlFlow::Continue;
            }
            paned.set_position((ratio * dimension as f64).round() as i32);
            glib::ControlFlow::Break
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
        click.connect_released(move |_, _, _, _| Page::new(&tw2, &name2).select_and_focus());
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

    /// Restores a user-selected tab's last pane; programmatic selection does not steal focus.
    pub(super) fn select_and_focus(&self) {
        self.select();
        let tw = self.window;
        let name = self.name.clone();
        let terminal = tw
            .page_focus
            .borrow()
            .get(&name)
            .and_then(glib::WeakRef::upgrade)
            .or_else(|| tw.stack.child_by_name(&name).and_then(|page| PaneView::first(&page)));
        let Some(terminal) = terminal else { return };
        let stack = tw.stack.downgrade();
        terminal.add_tick_callback(move |terminal, _| {
            let Some(stack) = stack.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if stack.visible_child_name().as_deref() != Some(name.as_str()) {
                return glib::ControlFlow::Break;
            }
            if !terminal.is_mapped() {
                return glib::ControlFlow::Continue;
            }
            terminal.grab_focus();
            glib::ControlFlow::Break
        });
    }

    pub(crate) fn close(&self) {
        let tw = self.window;
        let name = self.name.as_str();
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
            let clear_focus = page_owns_focus(tw.focused.borrow().as_ref(), &terminals);
            if clear_focus {
                tw.focused.borrow_mut().take();
            }
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
        let Some(i) = pos else { return };
        let Some(next) = tw.entries.borrow().get(i).map(|e| e.name.clone()) else {
            return;
        };
        let page = Self::new(tw, &next);
        if tw.search.entry.has_focus() {
            page.select();
            return;
        }
        page.select_and_focus();
    }

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

    /// Every VTE terminal in `w`'s subtree; a headless verification run has
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
#[allow(unsafe_code)]
mod focus_ownership_tests {
    use super::*;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    #[test]
    fn removed_page_clears_only_focus_owned_by_that_page() {
        assert!(page_owns_focus(Some(&2), &[1, 2, 3]));
        assert!(!page_owns_focus(Some(&4), &[1, 2, 3]));
        assert!(!page_owns_focus::<i32>(None, &[1, 2, 3]));
    }

    #[test]
    fn tab_selection_and_close_route_utf8_paste_only_to_the_visible_pty() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let workspace = WorkspaceConfig::new("dev", "alpine:3.20", hl_ws::Arch::Arm64);
            let tw = Window::bench(&workspace);
            let root = tw.stack.root().unwrap().downcast::<gtk::Window>().unwrap();
            root.set_child(gtk::Widget::NONE);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&tw.stack));
            overlay.add_overlay(&tw.search.bar);
            root.set_child(Some(&overlay));
            root.present();
            let overview = gtk::Label::new(Some("overview"));
            Tabs::new(&tw).add("overview", None, &overview, false);

            let (a, a_slave) = terminal_with_pty();
            let a_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            a_page.append(&a);
            let a_name = Tabs::new(&tw).add("a", None, &a_page, true);
            PaneFocus::wire(&tw, &a);

            let (b_first, b_first_slave) = terminal_with_pty();
            let (b, b_slave) = terminal_with_pty();
            let b_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            b_page.append(&b_first);
            assert!(PaneSplit::insert(
                b_first.upcast_ref::<gtk::Widget>(),
                gtk::Orientation::Horizontal,
                b.upcast_ref::<gtk::Widget>()
            ));
            let b_name = Tabs::new(&tw).add("b", None, &b_page, true);
            PaneFocus::wire(&tw, &b_first);
            PaneFocus::wire(&tw, &b);

            Page::new(&tw, &b_name).select_and_focus();
            await_focus(&tw, &b_first);
            b.grab_focus();
            await_focus(&tw, &b);
            Page::new(&tw, &a_name).select_and_focus();
            await_focus(&tw, &a);
            paste_and_expect(&tw, &a, a_slave.as_raw_fd(), "α from a");
            assert_quiet(b_slave.as_raw_fd());
            assert_quiet(b_first_slave.as_raw_fd());

            // This is the same route the tab-strip click handler invokes.
            Page::new(&tw, &b_name).select_and_focus();
            await_focus(&tw, &b);
            paste_and_expect(&tw, &b, b_slave.as_raw_fd(), "β from b");
            assert_quiet(a_slave.as_raw_fd());
            assert_quiet(b_first_slave.as_raw_fd());

            // A programmatic page change must not take focus from the search
            // overlay merely because the new page contains a terminal.
            tw.search.bar.set_visible(true);
            tw.search.entry.grab_focus();
            await_widget_focus(&root, tw.search.entry.upcast_ref());
            Page::new(&tw, &a_name).select();
            settle_frames(2);
            assert!(owns_focus(&root, tw.search.entry.upcast_ref()));
            tw.search.bar.set_visible(false);

            // A focus request queued for a page is cancelled if another page
            // becomes visible before the mapped-frame callback runs.
            Page::new(&tw, &a_name).select_and_focus();
            Page::new(&tw, &b_name).select();
            settle_frames(2);
            assert!(!a.has_focus());

            b.grab_focus();
            await_focus(&tw, &b);
            Page::new(&tw, &a_name).select_and_focus();
            await_focus(&tw, &a);
            Page::new(&tw, &a_name).close();
            await_focus(&tw, &b);
            paste_and_expect(&tw, &b, b_slave.as_raw_fd(), "終 after close");
            assert_quiet(a_slave.as_raw_fd());
            assert_quiet(b_first_slave.as_raw_fd());
        });
        if !ran {
            println!("skipped: no display connection");
        }
    }

    fn terminal_with_pty() -> (vte4::Terminal, std::os::fd::OwnedFd) {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: openpty initializes both descriptors; ownership is immediately adopted below.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &raw mut master,
                    &raw mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        // SAFETY: openpty returned these two unique owned descriptors.
        let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(master) };
        let slave = unsafe { std::os::fd::OwnedFd::from_raw_fd(slave) };
        let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: slave is live; tcgetattr initializes attrs and tcsetattr only borrows it.
        assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), attrs.as_mut_ptr()) }, 0);
        // SAFETY: successful tcgetattr initialized attrs.
        let mut attrs = unsafe { attrs.assume_init() };
        // SAFETY: attrs is an initialized termios exclusively owned here.
        unsafe { libc::cfmakeraw(&raw mut attrs) };
        // SAFETY: slave is live and attrs remains borrowed for this call only.
        assert_eq!(
            unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &raw const attrs) },
            0
        );
        let pty = vte4::Pty::foreign_sync(master, gio::Cancellable::NONE).unwrap();
        let terminal = vte4::Terminal::new();
        terminal.set_pty(Some(&pty));
        (terminal, slave)
    }

    fn await_focus(tw: &Rc<TermWin>, expected: &vte4::Terminal) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            while glib::MainContext::default().iteration(false) {}
            if expected.has_focus() && tw.focused.borrow().as_ref() == Some(expected) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "selected terminal never acquired focus: mapped={} visible={} focusable={} root={} page={:?}",
                expected.is_mapped(),
                expected.is_visible(),
                expected.is_focusable(),
                expected.root().is_some(),
                tw.stack.visible_child_name()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn paste_and_expect(tw: &TermWin, terminal: &vte4::Terminal, fd: libc::c_int, text: &str) {
        terminal.clipboard().set_text(text);
        Clipboard::paste(tw);
        let mut received = vec![0_u8; text.len()];
        let mut offset = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while offset < received.len() {
            while glib::MainContext::default().iteration(false) {}
            let mut descriptor = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor is initialized and exclusively borrowed for poll.
            if unsafe { libc::poll(&raw mut descriptor, 1, 5) } > 0 {
                // SAFETY: fd is live and the remaining slice is writable for this call.
                let count = unsafe { libc::read(fd, received[offset..].as_mut_ptr().cast(), received.len() - offset) };
                assert!(count > 0);
                offset += count as usize;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "paste received only {offset} bytes"
            );
        }
        assert_eq!(received, text.as_bytes());
    }

    fn assert_quiet(fd: libc::c_int) {
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor is initialized and exclusively borrowed for poll.
        assert_eq!(
            unsafe { libc::poll(&raw mut descriptor, 1, 20) },
            0,
            "paste reached a hidden terminal"
        );
    }

    fn settle_frames(count: usize) {
        for _ in 0..count {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn await_widget_focus(root: &gtk::Window, widget: &gtk::Widget) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            while glib::MainContext::default().iteration(false) {}
            if owns_focus(root, widget) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "widget never acquired focus: mapped={} visible={} focusable={} root={}",
                widget.is_mapped(),
                widget.is_visible(),
                widget.is_focusable(),
                widget.root().is_some()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn owns_focus(root: &gtk::Window, owner: &gtk::Widget) -> bool {
        let mut focused: Option<gtk::Widget> = gtk::prelude::RootExt::focus(root);
        while let Some(widget) = focused {
            if widget == *owner {
                return true;
            }
            focused = widget.parent();
        }
        false
    }
}

#[cfg(test)]
mod split_position_tests {
    use super::*;

    #[test]
    fn a_hidden_tab_restores_when_it_is_first_allocated() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let stack = gtk::Stack::new();
            let visible = gtk::Label::new(Some("visible"));
            let hidden = split();
            stack.add_named(&visible, Some("visible"));
            stack.add_named(&hidden, Some("hidden"));
            stack.set_visible_child_name("visible");
            let window = gtk::Window::new();
            window.set_default_size(800, 400);
            window.set_child(Some(&stack));

            SplitPosition::restore(&hidden, SplitDir::Horizontal, 0.25);
            window.present();
            settle_for(std::time::Duration::from_millis(250));
            assert!(
                hidden.width() <= 1,
                "hidden split was unexpectedly allocated: {}",
                hidden.width()
            );

            stack.set_visible_child_name("hidden");
            let width = await_ratio(&hidden, 0.25);
            assert_ne!(width, 0);
            window.close();
        });
        if !ran {
            println!("skipped: no display connection");
        }
    }

    #[test]
    fn an_allocated_split_restores_once_and_never_overrides_a_user_drag() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let paned = split();
            let window = gtk::Window::new();
            window.set_default_size(800, 400);
            window.set_child(Some(&paned));
            window.present();
            await_dimension(&paned);

            SplitPosition::restore(&paned, SplitDir::Horizontal, 0.25);
            let width = await_ratio(&paned, 0.25);
            let dragged = (f64::from(width) * 0.70).round() as i32;
            paned.set_position(dragged);
            settle_frames(4);
            assert_eq!(
                paned.position(),
                dragged,
                "restore callback remained armed after applying once"
            );
            window.close();
        });
        if !ran {
            println!("skipped: no display connection");
        }
    }

    fn split() -> gtk::Paned {
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_hexpand(true);
        paned.set_vexpand(true);
        paned.set_start_child(Some(&gtk::Label::new(Some("start"))));
        paned.set_end_child(Some(&gtk::Label::new(Some("end"))));
        paned
    }

    fn await_dimension(paned: &gtk::Paned) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            while glib::MainContext::default().iteration(false) {}
            if paned.width() > 1 {
                return paned.width();
            }
            assert!(std::time::Instant::now() < deadline, "split was never allocated");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn await_ratio(paned: &gtk::Paned, ratio: f64) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            while glib::MainContext::default().iteration(false) {}
            let width = paned.width();
            let expected = (f64::from(width) * ratio).round() as i32;
            if width > 1 && (paned.position() - expected).abs() <= 1 {
                return width;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "width={width} position={}",
                paned.position()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn settle_frames(count: usize) {
        let mut frames = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while frames < count {
            if glib::MainContext::default().iteration(false) {
                frames += 1;
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(
                std::time::Instant::now() < deadline,
                "toolkit produced fewer than {count} events"
            );
        }
    }

    fn settle_for(duration: std::time::Duration) {
        let deadline = std::time::Instant::now() + duration;
        while std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
