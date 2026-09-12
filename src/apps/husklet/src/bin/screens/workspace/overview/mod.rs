use crate::*;

use screens::workspace::extensions::{Console, Gallery, Shelf, Surfaces};

pub(crate) struct Overview<'a> {
    workspace: &'a WorkspaceConfig,
    page: Option<screens::workspace::Page>,
    /// The terminal window this overview is a tab of, when it is one. An
    /// extension's pane requests are answered from there and nowhere else.
    window: Option<&'a Rc<screens::workspace::terminal::TermWin>>,
}

impl<'a> Overview<'a> {
    pub(crate) fn new(workspace: &'a WorkspaceConfig, page: Option<screens::workspace::Page>) -> Self {
        Self {
            workspace,
            page,
            window: None,
        }
    }

    /// Binds the overview to the terminal window it is a tab of, which is what
    /// lets an extension reach panes.
    pub(crate) const fn within(mut self, window: &'a Rc<screens::workspace::terminal::TermWin>) -> Self {
        self.window = Some(window);
        self
    }

    /// The surface one extension draws into, fed by a host of its own.
    ///
    /// Nothing here blocks and nothing here fails. Reading the installation,
    /// reaching the container daemon, and binding the socket all happen on the
    /// host's own thread, so a workspace whose daemon is slow — or whose
    /// extension is disabled — costs the main loop nothing. An extension that
    /// is not running is told so through the same banner path a stopped one
    /// uses, over the empty surface it already has.
    ///
    /// The interface is placed inside a holder that stays on the shell: an
    /// extension may move its interface into a terminal pane, and the page it
    /// came from has to have somewhere to put it back.
    fn surface(
        workspace: &WorkspaceConfig,
        name: &hl_extension::ExtensionName,
        providers: &[hl_extension::PaneProvider],
        terminal: &std::sync::Arc<dyn hl_extension::port::TerminalSurface + Send + Sync>,
        events: hl::extension::Events,
        gallery: &Gallery,
        faulted: Rc<dyn Fn(u32)>,
    ) -> gtk::Widget {
        use hl::extension::{Order, Report};
        use screens::workspace::extension::{Delivery, Signal};

        let (post, deliveries) = screens::workspace::extension::channel();
        let notification_name = name.to_string();
        // The two halves were built apart and carry the same three cases under
        // their own names, because the page lives in this binary and the host
        // lives in the library; this is the whole of the translation.
        let audience = Box::new(move |report| {
            let delivery = match report {
                Report::Reset => Delivery::Reset,
                Report::Frame(frame) => Delivery::FrameAt {
                    slot: frame.slot,
                    frame: frame.frame,
                },
                Report::Source(mutation) => Delivery::SourceAt {
                    slot: mutation.slot,
                    mutation: mutation.mutation,
                },
                Report::Notification(notification) => {
                    let message =
                        gtk::gio::Notification::new(&notification_title(&notification_name, &notification.title));
                    message.set_body(Some(&notification.body));
                    if let Some(application) = gtk::gio::Application::default() {
                        application
                            .send_notification(Some(&notification_id(&notification_name, &notification.id)), &message);
                    }
                    return;
                }
                Report::Loss(reason) => Delivery::Loss(reason),
                Report::Fault { restarts } => Delivery::Fault { restarts },
            };
            // A page that has gone away is not a failure: the host is about
            // to be dropped with it.
            drop(post.send(delivery));
        });
        #[cfg(debug_assertions)]
        let host = if name.as_str() == "top" {
            if let Some(entrypoint) = AppConfig::get().local_extension.as_deref() {
                hl::extension::Host::local_extension(
                    workspace,
                    name,
                    std::sync::Arc::clone(terminal),
                    events,
                    entrypoint,
                    AppConfig::get().overview_pane.as_deref().and_then(top_section),
                    audience,
                )
            } else {
                hl::extension::Host::extension(workspace, name, std::sync::Arc::clone(terminal), events, audience)
            }
        } else {
            hl::extension::Host::extension(workspace, name, std::sync::Arc::clone(terminal), events, audience)
        };
        #[cfg(not(debug_assertions))]
        let host = hl::extension::Host::extension(workspace, name, std::sync::Arc::clone(terminal), events, audience);
        let host = std::rc::Rc::new(host);
        // The page never names the host, so the sink is where the two vocabularies
        // meet: one enum for what a person did, one for what the extension said.
        let ordered = std::rc::Rc::clone(&host);
        let selected = std::rc::Rc::new(move |selection| ordered.accept(Order::PaneProvider(selection)));
        let stopping = Rc::downgrade(&host);
        let sink = std::rc::Rc::new(move |signal: Signal| match signal {
            Signal::Interaction(event) => host.accept(Order::Interaction(event)),
            Signal::InteractionAt { slot, event } => {
                host.accept(Order::InteractionAt(hl_extension::SurfaceEvent { slot, event }))
            }
            Signal::Retry => host.accept(Order::Retry),
        });
        let ready_gallery = gallery.clone();
        let ready_name = name.to_string();
        let ready_generation = Rc::new(std::cell::Cell::new(None));
        let published_generation = Rc::clone(&ready_generation);
        let ready = Rc::new(move || {
            if let Some(generation) = published_generation.get() {
                ready_gallery.ready(&ready_name, generation);
            }
        });
        let loading = if name.as_str() == "top" {
            "Starting workspace overview…".to_owned()
        } else {
            format!("Starting {name}…")
        };
        let (widget, page) =
            screens::workspace::extension::Interface::with_lifecycle_label(deliveries, sink, faulted, ready, &loading);
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.set_hexpand(true);
        holder.set_vexpand(true);
        holder.append(&widget);
        let generation = gallery.enrol(name.as_str(), &widget, &holder, providers, selected);
        ready_generation.set(Some(generation));
        gallery.enrol_shutdown(
            name.as_str(),
            Rc::new(move || {
                if let Some(host) = stopping.upgrade() {
                    host.request_stop();
                }
            }),
        );
        let page = page.install();
        let weak = Rc::downgrade(&page);
        gallery.enrol_panes(
            name.as_str(),
            Rc::new(move |slot| {
                weak.upgrade()
                    .map(|page| page.borrow_mut().pane(slot))
                    .unwrap_or_else(|| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast())
            }),
        );
        let weak = Rc::downgrade(&page);
        gallery.enrol_retirement(
            name.as_str(),
            Rc::new(move |slot| {
                if let Some(page) = weak.upgrade() {
                    page.borrow_mut().retire(slot);
                }
            }),
        );
        let weak = Rc::downgrade(&page);
        let semantics = Rc::new(move |slot: &str| {
            weak.upgrade()
                .ok_or_else(|| hl_extension::HostError::Absent("extension surface closed".into()))?
                .borrow()
                .semantics(slot)
        });
        let weak = Rc::downgrade(&page);
        let action = Rc::new(move |slot: &str, request: &hl_extension::PaneSemanticAction| {
            weak.upgrade()
                .ok_or_else(|| hl_extension::HostError::Absent("extension surface closed".into()))?
                .borrow()
                .semantic_action_at(slot, request)
        });
        gallery.enrol_semantics(name.as_str(), semantics, action);
        holder.upcast()
    }

    /// The workspace's extensions, as pages on the shell.
    ///
    /// A roster that cannot be read is reported on the page rather than hidden:
    /// an empty list and unreadable storage look the same to a person, and only
    /// one of them means their extensions are gone.
    fn shelf(
        workspace: &WorkspaceConfig,
        view: &Rc<screens::workspace::View>,
        relay: &Rc<hl::extension::Relay>,
        gallery: &Gallery,
        window: Option<&Rc<screens::workspace::terminal::TermWin>>,
    ) -> Option<Rc<Shelf>> {
        let roster = match hl::extension::Roster::workspace(workspace) {
            Ok(roster) => Rc::new(RefCell::new(roster)),
            Err(refusal) => {
                hl_log::hl_error!(hl_log::tag::RUNTIME, "workspace extensions: {refusal}");
                return None;
            }
        };
        let held = workspace.clone();
        let carried = Rc::clone(relay);
        let shown = gallery.clone();
        // Filled after the shelf is constructed. The surfaces it owns keep
        // only a weak route back, so lifecycle callbacks cannot form a cycle.
        let shelf_anchor = Rc::new(RefCell::new(std::rc::Weak::<Shelf>::new()));
        let anchored = Rc::clone(&shelf_anchor);
        let observed = window.map(Rc::downgrade);
        // Each extension holds a port of its own, because a pane that draws an
        // interface has to name whose interface it draws and one shared port
        // could not say.
        let surfaces: Surfaces = Rc::new(move |entry| {
            let port: std::sync::Arc<dyn hl_extension::port::TerminalSurface + Send + Sync> =
                std::sync::Arc::new(carried.of(entry.name.as_str()));
            let providers = if entry.stage == hl_extension::Stage::Duty {
                entry.pane_providers.as_slice()
            } else {
                &[]
            };
            let name = entry.name.clone();
            let image_digest = entry.image_digest.clone();
            let anchored = Rc::clone(&anchored);
            let faulted = Rc::new(move |restarts| {
                if let Some(shelf) = anchored.borrow().upgrade() {
                    shelf.fault(&name, &image_digest, restarts);
                }
            });
            let events = observed
                .as_ref()
                .and_then(std::rc::Weak::upgrade)
                .map_or_else(hl::extension::Events::default, |window| window.observer());
            let surface = Self::surface(&held, &entry.name, providers, &port, events, &shown, faulted);
            if let Some(window) = observed.as_ref().and_then(std::rc::Weak::upgrade) {
                screens::workspace::terminal::PaneChooser::recover(&window, entry.name.as_str());
            }
            surface
        });
        let gallery_for_withdrawal = gallery.clone();
        let window = window.map(Rc::downgrade);
        let withdraw = Rc::new(move |name: &hl_extension::ExtensionName| {
            if let Some(window) = window.as_ref().and_then(std::rc::Weak::upgrade) {
                screens::workspace::terminal::PaneChooser::withdraw(&window, name.as_str());
            }
            gallery_for_withdrawal.withdraw(name.as_str());
        });
        let shelf = Shelf::with_lifecycle(view, workspace, &roster, surfaces, withdraw);
        shelf_anchor.replace(Rc::downgrade(&shelf));
        shelf.install();
        Some(shelf)
    }

    pub(crate) fn view(&self) -> gtk::Box {
        let ws = self.workspace;
        let semantics = screens::workspace::semantic::Registry::new("workspace");
        let view = Rc::new(screens::workspace::View::with_semantics([], semantics));
        // The terminal port an extension holds is a relay to whichever window is
        // drawing; the window answers it on its own tick, which is where the
        // widgets are.
        let (relay, errands) = hl::extension::Relay::open();
        let relay = Rc::new(relay);
        let gallery = Gallery::new();
        gallery.enrol_native(view.semantic_registry());
        // The window looks its panes' interfaces up here, so it must be told
        // where they are before a saved layout is restored into it.
        if let Some(window) = self.window {
            screens::workspace::terminal::Window::exhibit(window, gallery.clone());
        }
        let shelf = Rc::new(RefCell::new(Self::shelf(ws, &view, &relay, &gallery, self.window)));
        if shelf.borrow().is_none() {
            let recovery = gtk::Box::new(gtk::Orientation::Vertical, 8);
            recovery.set_halign(gtk::Align::Start);
            recovery.set_valign(gtk::Align::Start);
            recovery.set_margin_top(24);
            recovery.set_margin_start(24);
            recovery.set_margin_end(24);
            recovery.set_accessible_role(gtk::AccessibleRole::Alert);
            let title = gtk::Label::new(Some("Extensions unavailable"));
            title.add_css_class("title-2");
            title.set_xalign(0.0);
            let detail = gtk::Label::new(Some(
                "Husklet could not read this workspace's extensions. No extensions were changed.",
            ));
            detail.set_xalign(0.0);
            detail.set_wrap(true);
            detail.set_width_chars(1);
            detail.set_max_width_chars(56);
            let retry = gtk::Button::with_label("Retry extensions");
            retry.add_css_class("suggested-action");
            retry.set_halign(gtk::Align::Start);
            recovery.append(&title);
            recovery.append(&detail);
            recovery.append(&retry);
            view.attach("failure", "Unavailable", recovery.upcast_ref());

            let workspace = ws.clone();
            let held_view = Rc::clone(&view);
            let held_relay = Rc::clone(&relay);
            let held_gallery = gallery.clone();
            let held_window = self.window.map(Rc::downgrade);
            let held_shelf = Rc::clone(&shelf);
            retry.connect_clicked(move |button| {
                button.set_sensitive(false);
                let window = held_window.as_ref().and_then(std::rc::Weak::upgrade);
                let Some(recovered) = Self::shelf(&workspace, &held_view, &held_relay, &held_gallery, window.as_ref())
                else {
                    button.set_sensitive(true);
                    return;
                };
                recovered.install();
                held_shelf.replace(Some(recovered));
                held_view.detach("failure");
            });
        }
        if let Some(window) = self.window {
            Console::new(window, errands).install();
        }
        // The shell and its "Extensions" page are held here rather than weakly,
        // because the pages an extension is on are attached to the shell after
        // this returns and something has to keep it. Liveness is read from the
        // widget's own root instead: a page that once had a window and no
        // longer does is a window that closed.
        let held = Rc::clone(&view);
        let rooted = Cell::new(false);
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let live = held.widget.root().is_some();
            rooted.set(rooted.get() || live);
            if rooted.get() && !live {
                return glib::ControlFlow::Break;
            }
            // Reconciliation reads only a process-local counter while idle;
            // durable records are reopened after a lifecycle mutation.
            if let Some(shelf) = shelf.borrow().as_ref() {
                shelf.reconcile();
            }
            glib::ControlFlow::Continue
        });

        // Debug selection is fail-closed: an unavailable extension leaves the
        // first mounted extension selected.
        if let Some(p) = AppConfig::get().overview_pane.as_deref() {
            view.select_name(if top_section(p).is_some() { "top" } else { p });
        } else if let Some(page) = self.page {
            view.select_name(page.id());
        }
        view.widget.clone()
    }
}

fn top_section(name: &str) -> Option<&str> {
    matches!(
        name,
        "overview"
            | "workspace"
            | "extensions"
            | "containers"
            | "processes"
            | "executions"
            | "images"
            | "volumes"
            | "networks"
            | "terminals"
    )
    .then_some(name)
}

fn notification_title(extension: &str, title: &str) -> String {
    format!("{extension}: {title}")
}

fn notification_id(extension: &str, id: &str) -> String {
    format!("{extension}:{id}")
}

#[cfg(test)]
mod notification_tests {
    use super::{notification_id, notification_title, top_section};

    #[test]
    fn host_owns_visible_attribution_and_stable_replacement_identity() {
        assert_eq!(notification_title("indexer", "Complete"), "indexer: Complete");
        assert_eq!(notification_id("indexer", "build"), notification_id("indexer", "build"));
        assert_ne!(notification_id("indexer", "build"), notification_id("monitor", "build"));
    }

    #[test]
    fn debug_overview_routes_recognize_every_top_section_exactly() {
        for section in [
            "overview",
            "workspace",
            "extensions",
            "containers",
            "processes",
            "executions",
            "images",
            "volumes",
            "networks",
            "terminals",
        ] {
            assert_eq!(top_section(section), Some(section));
        }
        assert_eq!(top_section("storybook"), None);
        assert_eq!(top_section("extension"), None);
    }
}
