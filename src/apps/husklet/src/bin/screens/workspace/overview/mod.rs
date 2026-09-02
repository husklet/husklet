use crate::*;

mod poll;
mod process;
mod resources;
mod settings;
mod summary;
mod table;

pub(crate) use process::*;
pub(crate) use resources::*;

use poll::{spawn_overview_poller, Data};
use screens::workspace::extensions::{Catalogue, Console, Gallery, Inspection, Shelf, Surfaces};
use table::Table;

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
        gallery: &Gallery,
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
            Box::new(move |report| {
                let delivery = match report {
                    Report::Frame(frame) => Delivery::Frame(frame),
                    Report::Source(mutation) => Delivery::Source(mutation),
                    Report::Loss(reason) => Delivery::Loss(reason),
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
        let sink = std::rc::Rc::new(move |signal: Signal| match signal {
            Signal::Interaction(event) => host.accept(Order::Interaction(event)),
            Signal::Retry => host.accept(Order::Retry),
        });
        let (widget, page) = screens::workspace::extension::Interface::new(deliveries, sink);
        page.install();
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.set_hexpand(true);
        holder.set_vexpand(true);
        holder.append(&widget);
        gallery.enrol(name.as_str(), &widget, &holder, providers, selected);
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
        // Each extension holds a port of its own, because a pane that draws an
        // interface has to name whose interface it draws and one shared port
        // could not say.
        let surfaces: Surfaces = Rc::new(move |entry| {
            let port: std::sync::Arc<dyn hl_extension::port::TerminalSurface + Send + Sync> =
                std::sync::Arc::new(carried.of(entry.name.as_str()));
            Self::surface(&held, &entry.name, &entry.pane_providers, &port, &shown)
        });
        let shelf = Shelf::new(view, &roster, surfaces);
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
            let workspace = held.clone();
            let reference = reference.to_owned();
            std::thread::spawn(move || {
                hl::extension::Candidate::acquire(&workspace, &reference, &answered);
            });
            answer
        })
    }

    pub(crate) fn view(&self) -> gtk::Box {
        use screens::workspace::Page as WorkspacePage;

        let ws = self.workspace;

        // Live panes fed by a background poller over the workspace daemon's Unix socket.
        let data = std::sync::Arc::new(std::sync::Mutex::new(Data::loading()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        spawn_overview_poller(ws.name.clone(), self.shell_label(), data.clone(), stop.clone());
        let containers = Table::new(&["NAME", "IMAGE", "STATUS"]);
        let images = Table::new(&["REPOSITORY", "IMAGE ID", "SIZE"]);
        let volumes = Table::new(&["NAME", "DRIVER"]);
        let networks = Table::new(&["NAME", "DRIVER", "SCOPE"]);
        let (ppane, pbody) = live_proc_pane();
        let shelf = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let view = Rc::new(screens::workspace::View::new([
            (WorkspacePage::Overview, self.overview().upcast()),
            (WorkspacePage::Containers, containers.widget.clone().upcast()),
            (WorkspacePage::Images, images.widget.clone().upcast()),
            (WorkspacePage::Volumes, volumes.widget.clone().upcast()),
            (WorkspacePage::Networks, networks.widget.clone().upcast()),
            (WorkspacePage::Processes, ppane.upcast()),
            (WorkspacePage::Extensions, shelf.clone().upcast()),
            (WorkspacePage::Settings, self.settings().upcast()),
        ]));
        // The terminal port an extension holds is a relay to whichever window is
        // drawing; the window answers it on its own tick, which is where the
        // widgets are.
        let (relay, errands) = hl::extension::Relay::open();
        let relay = Rc::new(relay);
        let gallery = Gallery::new();
        // The window looks its panes' interfaces up here, so it must be told
        // where they are before a saved layout is restored into it.
        if let Some(window) = self.window {
            screens::workspace::terminal::Window::exhibit(window, gallery.clone());
        }
        let catalogue = Self::shelf(ws, &view, &relay, &gallery);
        if let Some(catalogue) = &catalogue {
            shelf.append(catalogue.widget());
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
        let workspace = ws.name.clone();
        let last = RefCell::new(None);
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let live = held.widget.root().is_some();
            rooted.set(rooted.get() || live);
            if rooted.get() && !live {
                stop.store(true, std::sync::atomic::Ordering::Release);
                return glib::ControlFlow::Break;
            }
            // The catalogue's own polling holds only a weak reference to
            // itself, so the page is kept alive here, beside the shell it is on.
            let _ = &catalogue;
            let d = data.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if last.borrow().as_ref() == Some(&d) {
                return glib::ControlFlow::Continue;
            }
            containers.fill(&d.containers, d.resources_error.as_deref());
            images.fill(&d.images, d.resources_error.as_deref());
            volumes.fill(&d.volumes, d.resources_error.as_deref());
            networks.fill(&d.networks, d.resources_error.as_deref());
            fill_proc_table(&pbody, &workspace, &d.processes, d.processes_error.as_deref());
            *last.borrow_mut() = Some(d);
            glib::ControlFlow::Continue
        });

        // Debug: HL_TERM_OVERVIEW_PAGE selects a overview pane for screenshotting.
        if let Some(p) = AppConfig::get().overview_pane.as_deref() {
            view.select_name(p);
        } else if let Some(page) = self.page {
            view.select_name(page.title());
        }
        view.widget.clone()
    }

    fn shell_label(&self) -> String {
        self.workspace
            .shell
            .as_deref()
            .map(str::trim)
            .filter(|shell| !shell.is_empty())
            .and_then(|shell| shell.split_whitespace().next())
            .map_or_else(
                || "bash".to_string(),
                |shell| shell.rsplit('/').next().unwrap_or(shell).to_string(),
            )
    }
}
