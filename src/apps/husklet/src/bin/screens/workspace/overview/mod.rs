use crate::*;

mod process;
mod resources;
mod settings;
mod summary;
mod table;

pub(crate) use process::*;
pub(crate) use resources::*;

use screens::workspace::extensions::{Catalogue, Console, Gallery, Inspection, PendingInspection, Shelf, Surfaces};

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
        // The two halves were built apart and carry the same three cases under
        // their own names, because the page lives in this binary and the host
        // lives in the library; this is the whole of the translation.
        let host = std::rc::Rc::new(hl::extension::Host::extension(
            workspace,
            name,
            std::sync::Arc::clone(terminal),
            events,
            Box::new(move |report| {
                let delivery = match report {
                    Report::Frame(frame) => Delivery::FrameAt {
                        slot: frame.slot,
                        frame: frame.frame,
                    },
                    Report::Source(mutation) => Delivery::SourceAt {
                        slot: mutation.slot,
                        mutation: mutation.mutation,
                    },
                    Report::Loss(reason) => Delivery::Loss(reason),
                    Report::Fault { restarts } => Delivery::Fault { restarts },
                };
                // A page that has gone away is not a failure: the host is about
                // to be dropped with it.
                drop(post.send(delivery));
            }),
        ));
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
        let (widget, page) = screens::workspace::extension::Interface::with_lifecycle(deliveries, sink, faulted, ready);
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
    ) -> Option<Rc<Catalogue>> {
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
            let anchored = Rc::clone(&anchored);
            let faulted = Rc::new(move |restarts| {
                if let Some(shelf) = anchored.borrow().upgrade() {
                    shelf.fault(&name, restarts);
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
        let cleanup_workspace = workspace.clone();
        let cleanup = Rc::new(move |entry: hl::extension::Entry| {
            let (sent, received) = std::sync::mpsc::channel();
            let workspace = cleanup_workspace.clone();
            std::thread::spawn(move || {
                let result = hl::extension::Workspace::remove_extension(&workspace, &entry.name);
                let _ = sent.send(result);
            });
            received
        });
        let shelf = Shelf::with_cleanup(view, &roster, surfaces, withdraw, cleanup);
        shelf_anchor.replace(Rc::downgrade(&shelf));
        shelf.install();
        Some(Catalogue::new(&shelf, Self::inspections(workspace)))
    }

    /// How the "Extensions" page reads an image.
    ///
    /// On a thread of its own, because reading a manifest means creating a
    /// container from the image and copying a file out of it, and the window
    /// has to keep drawing while that happens.
    fn inspections(workspace: &WorkspaceConfig) -> Inspection {
        let held = workspace.clone();
        Rc::new(move |reference: &str| {
            let (answered, answer) = std::sync::mpsc::channel();
            let cancellation = hl::extension::Cancellation::default();
            let worker_cancellation = cancellation.clone();
            let workspace = held.clone();
            let reference = reference.to_owned();
            std::thread::spawn(move || {
                hl::extension::Candidate::acquire_cancellable(&workspace, &reference, &answered, &worker_cancellation);
            });
            PendingInspection {
                events: answer,
                cancellation,
            }
        })
    }

    pub(crate) fn view(&self) -> gtk::Box {
        use screens::workspace::Page as WorkspacePage;

        let ws = self.workspace;

        let shelf = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let semantics = screens::workspace::semantic::Registry::new("workspace");
        let view = Rc::new(screens::workspace::View::with_semantics(
            [
                (WorkspacePage::Settings, self.settings(&semantics).upcast()),
                (WorkspacePage::Extensions, shelf.clone().upcast()),
            ],
            semantics,
        ));
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
        let catalogue = Self::shelf(ws, &view, &relay, &gallery, self.window);
        if let Some(catalogue) = &catalogue {
            catalogue.shelf().catalogue().append(catalogue.viewport());
            shelf.append(catalogue.shelf().content());
        } else {
            let failure = gtk::Label::new(Some(
                "Extensions could not be loaded. Settings remain available; reopen this workspace to retry.",
            ));
            failure.set_wrap(true);
            failure.add_css_class("error");
            shelf.append(&failure);
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
            // The catalogue's own polling holds only a weak reference to
            // itself, so the page is kept alive here, beside the shell it is on.
            let _ = &catalogue;
            glib::ControlFlow::Continue
        });

        // Debug selection is fail-closed: removed legacy page names leave the
        // initial Settings page selected.
        if let Some(p) = AppConfig::get().overview_pane.as_deref() {
            view.select_name(p);
        } else if let Some(page) = self.page {
            view.select_name(page.title());
        }
        view.widget.clone()
    }
}
