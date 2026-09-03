use super::*;

fn page_owns_focus<T: PartialEq>(focused: Option<&T>, page: &[T]) -> bool {
    focused.is_some_and(|focused| page.iter().any(|candidate| candidate == focused))
}

pub(crate) struct PaneFocus;

/// The small control that selects what the current pane draws.
pub(crate) struct PaneChooser;

/// Stable visual and layout identity of one leaf pane.
///
/// The occupant is the overlay's primary child and may change between a live
/// terminal and an extension surface. Splits, ratios, close, and persistence
/// address the overlay, so changing the occupant never changes topology.
pub(crate) struct PaneChrome;

impl PaneChrome {
    pub(crate) const CLASS: &'static str = "hl-pane";

    pub(crate) fn wrap(window: &Rc<TermWin>, occupant: &impl IsA<gtk::Widget>) -> gtk::Widget {
        let chrome = gtk::Overlay::new();
        chrome.add_css_class(Self::CLASS);
        chrome.set_hexpand(true);
        chrome.set_vexpand(true);
        chrome.set_child(Some(occupant));
        chrome.add_overlay(&PaneChooser::button(window));
        chrome.upcast()
    }

    pub(crate) fn is(widget: &gtk::Widget) -> bool {
        widget.is::<gtk::Overlay>() && widget.has_css_class(Self::CLASS)
    }

    pub(crate) fn occupant(widget: &gtk::Widget) -> Option<gtk::Widget> {
        widget.downcast_ref::<gtk::Overlay>()?.child()
    }
}

impl PaneChooser {
    const SEARCH_THRESHOLD: usize = 6;
    const TERMINAL_ICON: &'static str = "utilities-terminal-symbolic";
    const PROVIDER_ICON: &'static str = "view-grid-symbolic";

    /// A chooser whose contents are rebuilt when it opens.
    ///
    /// The button exists even before an extension is installed, so tabs that
    /// predate an installation immediately see its providers without being
    /// rebuilt. Re-reading the gallery on every opening also removes disabled
    /// or uninstalled providers without leaving stale actions behind.
    pub(crate) fn button(window: &Rc<TermWin>) -> gtk::MenuButton {
        let button = gtk::MenuButton::new();
        button.set_icon_name(Self::TERMINAL_ICON);
        button.set_tooltip_text(Some("Choose what this pane displays"));
        button.set_focusable(true);
        button.update_property(&[gtk::accessible::Property::Label("Choose pane content")]);
        button.add_css_class("flat");
        button.set_halign(gtk::Align::End);
        button.set_valign(gtk::Align::Start);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(|controller, key, _, _| {
            if matches!(
                key,
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::space | gtk::gdk::Key::Down
            ) {
                if let Some(button) = controller.widget().and_downcast::<gtk::MenuButton>() {
                    button.popup();
                    return gtk::glib::Propagation::Stop;
                }
            }
            gtk::glib::Propagation::Proceed
        });
        button.add_controller(keys);
        Self::populate(window, &button);
        let weak = Rc::downgrade(window);
        button.connect_notify_local(Some("active"), move |button, _| {
            if !button.is_active() {
                return;
            }
            let Some(window) = weak.upgrade() else { return };
            Self::populate(&window, button);
        });
        button
    }

    pub(crate) fn populate(window: &Rc<TermWin>, button: &gtk::MenuButton) {
        let providers = Window::gallery(window).map_or_else(Vec::new, |gallery| gallery.providers());
        // The menu button is an overlay child of one stable pane chrome. Bind
        // its actions to that chrome's slot instead of whichever terminal last
        // happened to own keyboard focus.
        let current = button
            .parent()
            .and_then(|parent| Panes::all(window).into_iter().find(|pane| pane.widget == parent));
        let target = current.as_ref().map(|pane| pane.slot.clone());
        let identity = current
            .as_ref()
            .and_then(|pane| Slots::new(window).surface(&pane.content));
        let selected_provider = identity.as_ref().and_then(|(_, extension, provider)| {
            provider
                .as_ref()
                .map(|provider| (extension.as_str(), provider.as_str()))
        });
        let current_label = selected_provider
            .and_then(|(extension, id)| {
                providers
                    .iter()
                    .find(|provider| provider.extension == extension && provider.id == id)
                    .map(|provider| format!("{} · {}", provider.title, provider.extension))
            })
            .unwrap_or_else(|| {
                identity
                    .as_ref()
                    .map_or_else(|| "Terminal".to_owned(), |(_, extension, _)| extension.clone())
            });
        let current_icon = selected_provider
            .and_then(|(extension, id)| {
                providers
                    .iter()
                    .find(|provider| provider.extension == extension && provider.id == id)
                    .and_then(|provider| provider.icon.as_deref())
            })
            .unwrap_or(if identity.is_none() {
                Self::TERMINAL_ICON
            } else {
                Self::PROVIDER_ICON
            });
        button.set_icon_name(current_icon);
        let accessible = format!("Choose pane content; currently showing {current_label}");
        button.update_property(&[
            gtk::accessible::Property::Label("Choose pane content"),
            gtk::accessible::Property::Description(&accessible),
        ]);
        button.set_tooltip_text(Some(&accessible));
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 6);
        choices.set_margin_top(10);
        choices.set_margin_bottom(10);
        choices.set_margin_start(10);
        choices.set_margin_end(10);
        choices.set_size_request(200, -1);

        let heading = gtk::Label::new(Some("Pane content"));
        heading.set_accessible_role(gtk::AccessibleRole::Heading);
        heading.add_css_class("heading");
        heading.set_xalign(0.0);
        choices.append(&heading);

        let status = gtk::Label::new(Some(&format!("Currently showing {current_label}")));
        status.add_css_class("dim-label");
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_max_width_chars(30);
        choices.append(&status);

        let terminal = gtk::Button::with_label("Terminal");
        terminal.set_tooltip_text(Some("Show this pane's terminal"));
        terminal.set_halign(gtk::Align::Fill);
        if identity.is_none() {
            terminal.add_css_class("suggested-action");
            terminal.update_property(&[gtk::accessible::Property::Label("Terminal, selected")]);
        }
        {
            let window = window.clone();
            let target = target.clone();
            let chooser = button.clone();
            terminal.connect_clicked(move |_| {
                if Self::terminal_in(&window, target.as_deref()) {
                    chooser.set_icon_name(Self::TERMINAL_ICON);
                    Self::dismiss(&window, &chooser);
                }
            });
        }
        choices.append(&terminal);

        if providers.is_empty() {
            let empty = gtk::Box::new(gtk::Orientation::Vertical, 2);
            empty.add_css_class("dim-label");
            let title = gtk::Label::new(Some("No extension views available"));
            title.set_xalign(0.0);
            let detail = gtk::Label::new(Some("Install or enable an extension with pane views to show it here."));
            detail.set_xalign(0.0);
            detail.set_wrap(true);
            empty.append(&title);
            empty.append(&detail);
            choices.append(&empty);
        } else {
            let search = (providers.len() >= Self::SEARCH_THRESHOLD).then(|| {
                let search = gtk::SearchEntry::new();
                search.set_placeholder_text(Some("Search extension views"));
                choices.append(&search);
                search
            });

            let mut groups: Vec<(gtk::Label, Vec<(gtk::Button, String)>)> = Vec::new();
            let mut current_extension = None;
            for provider in providers {
                if current_extension.as_deref() != Some(provider.extension.as_str()) {
                    let label = gtk::Label::new(Some(&provider.extension));
                    label.set_accessible_role(gtk::AccessibleRole::Heading);
                    label.add_css_class("caption");
                    label.add_css_class("dim-label");
                    label.set_xalign(0.0);
                    choices.append(&label);
                    groups.push((label, Vec::new()));
                    current_extension = Some(provider.extension.clone());
                }
                let choice = gtk::Button::with_label(&provider.title);
                choice.set_tooltip_text(Some(&format!("{} · {}", provider.extension, provider.id)));
                choice.set_halign(gtk::Align::Fill);
                choice.set_hexpand(true);
                if selected_provider == Some((provider.extension.as_str(), provider.id.as_str())) {
                    choice.add_css_class("suggested-action");
                    choice.update_property(&[gtk::accessible::Property::Label(&format!(
                        "{}, selected",
                        provider.title
                    ))]);
                }
                let identity = format!("{}\n{} {}", provider.extension, provider.title, provider.id).to_lowercase();
                let window = window.clone();
                let target = target.clone();
                let extension = provider.extension;
                let generation = provider.generation;
                let id = provider.id;
                let icon = provider.icon.unwrap_or_else(|| Self::PROVIDER_ICON.to_owned());
                let chooser = button.clone();
                choice.connect_clicked(move |_| {
                    if Self::provider_generation_in(&window, target.as_deref(), &extension, generation, &id) {
                        chooser.set_icon_name(&icon);
                        Self::dismiss(&window, &chooser);
                    }
                });
                choices.append(&choice);
                groups
                    .last_mut()
                    .expect("a provider has an extension group")
                    .1
                    .push((choice, identity));
            }
            if let Some(search) = search {
                search.connect_search_changed(move |search| {
                    let query = search.text().to_lowercase();
                    for (heading, choices) in &groups {
                        let mut any = false;
                        for (choice, identity) in choices {
                            let visible = query.is_empty() || identity.contains(&query);
                            choice.set_visible(visible);
                            any |= visible;
                        }
                        heading.set_visible(any);
                    }
                });
            }
        }
        let popover = gtk::Popover::new();
        popover.set_child(Some(&choices));
        button.set_popover(Some(&popover));
    }

    fn dismiss(window: &TermWin, chooser: &gtk::MenuButton) {
        if let Some(popover) = chooser.popover() {
            popover.popdown();
        }
        if let Some(terminal) = window.focused.borrow().as_ref().filter(|terminal| terminal.parent().is_some()) {
            terminal.add_tick_callback(|terminal, _| {
                if !terminal.is_mapped() {
                    return glib::ControlFlow::Continue;
                }
                terminal.grab_focus();
                glib::ControlFlow::Break
            });
        }
    }

    pub(crate) fn selected(window: &Rc<TermWin>) -> Option<Occupancy> {
        if let Some(terminal) = window.focused.borrow().as_ref() {
            if let Some(slot) = Slots::new(window).of(terminal) {
                if let Some(pane) = Panes::at(window, &slot) {
                    return Some(pane);
                }
            }
        }
        Panes::under(window, &window.stack.visible_child()?).into_iter().next()
    }

    pub(crate) fn provider(window: &Rc<TermWin>, extension: &str, provider: &str) {
        let _ = Self::provider_in(window, None, extension, provider);
    }

    pub(crate) fn provider_in(window: &Rc<TermWin>, slot: Option<&str>, extension: &str, provider: &str) -> bool {
        let Some(gallery) = Window::gallery(window) else {
            return false;
        };
        let Some(generation) = gallery.generation(extension) else {
            return false;
        };
        Self::provider_generation_in(window, slot, extension, generation, provider)
    }

    fn provider_generation_in(
        window: &Rc<TermWin>,
        slot: Option<&str>,
        extension: &str,
        generation: u64,
        provider: &str,
    ) -> bool {
        let Some(gallery) = Window::gallery(window) else {
            return false;
        };
        if !gallery.offers_at(extension, generation, provider) {
            return false;
        }
        let Some(current) = slot
            .and_then(|slot| Panes::at(window, slot))
            .or_else(|| Self::selected(window))
        else {
            return false;
        };
        if !PaneSwap::can_replace(&current.content) {
            return false;
        }
        let previous_surface = (current.occupant == hl_extension::port::Occupant::Surface)
            .then(|| {
                Slots::new(window)
                    .surface(&current.content)
                    .map(|(_, extension, provider)| (extension, provider))
            })
            .flatten();
        let displaced = current.content.clone().downcast::<vte4::Terminal>().ok();
        // A shell is kept locally until replacement succeeds: a failed swap
        // must leave both layout and displaced-shell registry alone.
        if current.occupant == hl_extension::port::Occupant::Surface {
            Surface::retire(window, &current.content);
            Slots::new(window).release(&current.content);
        }
        let surface = Surface::build(window, extension, Some(provider), current.slot.clone());
        if PaneSwap::replace(&current.content, &surface) {
            if let Some(terminal) = displaced {
                window.displaced.borrow_mut().insert(current.slot.clone(), terminal);
            }
            gallery.select_at(extension, generation, provider, &current.slot);
            return true;
        }
        // The parent changed between preflight and replacement. Undo every
        // borrow/registration and put the old interface back exactly where it
        // was; the terminal case never entered the displaced registry.
        Surface::discard(window, &surface);
        if let Some((previous, provider)) = previous_surface {
            Slots::new(window).enrol(&current.content, current.slot, previous.clone(), provider);
            Surface::restore(window, &previous, &current.content);
        }
        false
    }

    pub(crate) fn terminal(window: &Rc<TermWin>) {
        let _ = Self::terminal_in(window, None);
    }

    pub(crate) fn terminal_in(window: &Rc<TermWin>, slot: Option<&str>) -> bool {
        let Some(current) = slot
            .and_then(|slot| Panes::at(window, slot))
            .or_else(|| Self::selected(window))
        else {
            return false;
        };
        Self::terminal_at(window, &current, true)
    }

    /// Restores every pane occupied by one extension without changing layout
    /// or focus. Lifecycle withdrawal must not leave provider authority in the
    /// live or subsequently persisted pane tree.
    pub(crate) fn withdraw(window: &Rc<TermWin>, extension: &str) {
        let held: Vec<_> = Panes::all(window)
            .into_iter()
            .filter(|pane| {
                Slots::new(window)
                    .surface(&pane.content)
                    .is_some_and(|(_, held, _)| held == extension)
            })
            .collect();
        for pane in held {
            let _ = Self::terminal_at(window, &pane, false);
        }
    }

    /// Rehydrates tombstones after an extension is enabled again. A provider
    /// removed from the new manifest remains frozen.
    pub(crate) fn recover(window: &Rc<TermWin>, extension: &str) {
        let Some(gallery) = Window::gallery(window) else { return };
        let held: Vec<_> = Panes::all(window)
            .into_iter()
            .filter_map(|pane| {
                let (_, owner, provider) = Slots::new(window).surface(&pane.content)?;
                (owner == extension && provider.as_deref().map_or(true, |id| gallery.offers(extension, id)))
                    .then_some((pane.content, pane.slot, provider))
            })
            .collect();
        for (pane, slot, provider) in held {
            Surface::restore(window, extension, &pane);
            if let Some(provider) = provider {
                gallery.select(extension, &provider, &slot);
            }
        }
    }

    fn terminal_at(window: &Rc<TermWin>, current: &Occupancy, focus: bool) -> bool {
        if current.occupant != hl_extension::port::Occupant::Surface {
            return false;
        }
        if !PaneSwap::can_replace(&current.content) {
            return false;
        }
        let identity = Slots::new(window)
            .surface(&current.content)
            .map(|(_, extension, provider)| (extension, provider));
        let Some(terminal) = window.displaced.borrow_mut().remove(&current.slot) else {
            return false;
        };
        Surface::retire(window, &current.content);
        Slots::new(window).release(&current.content);
        if PaneSwap::replace(&current.content, terminal.upcast_ref()) {
            if focus {
                terminal.grab_focus();
            }
            return true;
        }
        if let Some((extension, provider)) = identity {
            Slots::new(window).enrol(&current.content, current.slot.clone(), extension.clone(), provider);
            Surface::restore(window, &extension, &current.content);
        }
        window.displaced.borrow_mut().insert(current.slot.clone(), terminal);
        false
    }
}

struct PaneSwap;

impl PaneSwap {
    fn can_replace(old: &gtk::Widget) -> bool {
        let Some(parent) = old.parent() else { return false };
        if parent.is::<gtk::Box>() {
            return true;
        }
        if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
            return overlay.child().as_ref() == Some(old);
        }
        parent
            .downcast_ref::<gtk::Paned>()
            .is_some_and(|paned| paned.start_child().as_ref() == Some(old) || paned.end_child().as_ref() == Some(old))
    }

    fn replace(old: &gtk::Widget, new: &gtk::Widget) -> bool {
        if !Self::can_replace(old) {
            return false;
        }
        let Some(parent) = old.parent() else { return false };
        if let Some(container) = parent.downcast_ref::<gtk::Box>() {
            container.remove(old);
            container.append(new);
            return true;
        }
        if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
            overlay.set_child(Some(new));
            return true;
        }
        let Some(paned) = parent.downcast_ref::<gtk::Paned>() else {
            return false;
        };
        if paned.start_child().as_ref() == Some(old) {
            paned.set_start_child(Some(new));
        } else if paned.end_child().as_ref() == Some(old) {
            paned.set_end_child(Some(new));
        } else {
            return false;
        }
        true
    }
}

impl PaneFocus {
    pub(crate) fn wire(tw: &Rc<TermWin>, terminal: &vte4::Terminal) {
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
    pub(crate) fn build<L: PaneLauncher>(
        session: &WindowSession<'_>,
        node: &PaneNode,
        storage: &std::path::Path,
        pids: &mut Vec<Rc<Cell<i32>>>,
        launcher: &L,
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
                let (term, pid) = make_terminal_with(tw, pane.cwd.clone(), history, &slot, launcher);
                pids.push(pid);
                (PaneChrome::wrap(tw, &term), Some(term))
            }
            PaneNode::Surface(pane) => {
                // Reuse the pane's saved slot so an extension addressing its own
                // pane still finds it after a restart. Keep a live terminal
                // displaced behind it, just as an in-session provider switch
                // does, so late or absent providers never remove the escape
                // hatch from this leaf.
                let slot = Slots::new(tw).adopt(pane.slot.as_deref());
                let (terminal, pid) = make_terminal_with(tw, None, None, &slot, launcher);
                pids.push(pid);
                tw.displaced.borrow_mut().insert(slot.clone(), terminal);
                let surface = Surface::build(tw, &pane.extension, pane.provider.as_deref(), slot.clone());
                if let (Some(provider), Some(gallery)) = (pane.provider.as_deref(), Window::gallery(tw)) {
                    gallery.select(&pane.extension, provider, &slot);
                }
                (PaneChrome::wrap(tw, &surface), None)
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
                let (wa, fa) = session.build_pane_widget(a, storage, pids, launcher);
                let (wb, fb) = session.build_pane_widget(b, storage, pids, launcher);
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
        self.add_with_persistence(title, icon, content, closable, true)
    }

    fn add_with_persistence(
        &self,
        title: &str,
        icon: Option<&str>,
        content: &impl IsA<gtk::Widget>,
        closable: bool,
        persisted: bool,
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
            title: lbl,
            persisted,
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
        paneroot.append(&PaneChrome::wrap(tw, &term));
        let name = self.add(&format!("shell {n}"), None, &paneroot, true);
        tw.pids.borrow_mut().entry(name.clone()).or_default().push(pid);
        term.grab_focus();
        name
    }

    pub(crate) fn container_terminal(&self, container: &str, command: &[String]) -> String {
        let tw = self.window;
        let paneroot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        paneroot.set_hexpand(true);
        paneroot.set_vexpand(true);
        let slot = Slots::new(tw).allocate();
        let (term, pid) = make_container_terminal_ex(tw, &slot, container, command);
        paneroot.append(&PaneChrome::wrap(tw, &term));
        let title = format!("container {}", &container[..container.len().min(12)]);
        let name = self.add_with_persistence(&title, None, &paneroot, true, false);
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

    pub(crate) fn name(&self) -> &str {
        &self.name
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
        let Some(slot) = Slots::new(tw).of(&old) else { return };
        let Some(pane) = Panes::at(tw, &slot) else { return };
        Self::split_at(tw, &pane, &old, orient);
    }

    /// Divide a live pane while preserving its current occupant. A surface's
    /// displaced terminal remains the cwd source, while topology authority is
    /// the stable pane chrome and slot.
    pub(crate) fn split_at(
        tw: &Rc<TermWin>,
        pane: &Occupancy,
        old: &vte4::Terminal,
        orient: gtk::Orientation,
    ) {
        let page = Page::of(tw, &pane.widget).map(|page| page.name);
        // OSC-7: split panes inherit the source pane's cwd. A fresh split gets a fresh slot; never restores.
        let split_cwd = old
            .current_directory_uri()
            .and_then(|u| session::WorkingDirectory::from_osc7(&u).map(hl_ws_term::WorkingDirectory::into_string));
        let (new, pid) = make_terminal_ex(tw, split_cwd, None, &Slots::new(tw).allocate());
        if let Some(name) = &page {
            tw.pids.borrow_mut().entry(name.clone()).or_default().push(pid);
        }
        let wrapped = PaneChrome::wrap(tw, &new);
        if Panes::divide(tw, &pane.slot, orient, &wrapped) {
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
        if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
            overlay.set_child(gtk::Widget::NONE);
            Self::fill(&paned, old, new);
            overlay.set_child(Some(&paned));
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
    fn attached_container_tab_is_visible_closable_and_never_persisted() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let workspace = WorkspaceConfig::new("attach-test", "alpine:3.20", hl_ws::Arch::Amd64);
            let tw = Window::bench(&workspace);
            let overview = gtk::Label::new(Some("overview"));
            Tabs::new(&tw).add("overview", None, &overview, false);
            let id = "a".repeat(64);
            let tab = Tabs::new(&tw).container_terminal(&id, &["sh".into(), "-i".into()]);
            assert_eq!(tw.stack.visible_child_name().as_deref(), Some(tab.as_str()));
            let persisted = tw
                .entries
                .borrow()
                .iter()
                .find(|entry| entry.name == tab)
                .map(|entry| entry.persisted)
                .unwrap();
            assert!(!persisted, "attachment tabs must not enter session restore state");
            Page::new(&tw, &tab).close();
            assert!(tw.entries.borrow().iter().all(|entry| entry.name != tab));
            tw.closing.set(true);
        });
        if !ran {
            println!("skipped: no display connection");
        }
    }

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
