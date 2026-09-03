//! Every extension a workspace has, inside the workspace's Extensions page.
//!
//! One extension gets one sidebar page: the interface it draws itself. Husklet
//! owns lifecycle controls and keeps them together on the central Extensions
//! page, so installing an extension does not double the sidebar.
//!
//! What a page is built from is injected rather than reached for: the surface
//! builder and the image inspection are both handed in. That is what lets the
//! whole shelf — listing, selecting, enabling, removing, and consenting to an
//! image — be exercised with no container daemon and no sidecar.

mod console;
mod directory;
mod gallery;
mod settings;

#[cfg(test)]
mod test;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

use gtk::prelude::*;
use hl::extension::{Acquisition, Cancellation, Entry, Roster};
use hl_extension::{ExtensionName, Stage};
use hl_ws::storage::Directory;

pub(crate) use console::Console;
pub use directory::Catalogue;
pub use gallery::Gallery;

use super::View;

/// The roster of one workspace, shared by every page that acts on it.
pub type Shared = Rc<RefCell<Roster<Directory>>>;

/// How one extension's own page is built.
///
/// A closure because the real one owns a host on a thread of its own, and a
/// test wants a plain widget it can look at.
pub type Surfaces = Rc<dyn Fn(&Entry) -> gtk::Widget>;

/// How an image reference is read into something a person can be asked about.
///
/// The answer arrives on a channel because reading a manifest means reaching a
/// container daemon, and the main loop must keep drawing while that happens.
pub struct PendingInspection {
    pub events: Receiver<Acquisition>,
    pub cancellation: Cancellation,
}

impl PendingInspection {
    pub fn detached(events: Receiver<Acquisition>) -> Self {
        Self {
            events,
            cancellation: Cancellation::default(),
        }
    }
}

pub type Inspection = Rc<dyn Fn(&str) -> PendingInspection>;

const CATALOGUE: &str = "catalogue";

/// Restores terminal panes before one extension's interface page is rebuilt or
/// removed. The shelf names lifecycle intent; the terminal owns pane mechanics.
pub type Withdraw = Rc<dyn Fn(&ExtensionName)>;

/// Starts deletion of the exact managed sidecar described by an installed
/// entry. Runtime work stays off GTK; the receiver carries the visible result.
pub type Cleanup = Rc<dyn Fn(Entry) -> Receiver<Result<(), String>>>;

/// Where a workspace's extensions live on the shell.
pub struct Shelf {
    /// Held weakly: the pages this shelf builds live inside the shell, and
    /// their actions hold the shelf, so a strong reference here would be a
    /// cycle that outlives the window.
    view: std::rc::Weak<View>,
    roster: Shared,
    surfaces: Surfaces,
    pages: gtk::Stack,
    catalogue: gtk::Box,
    withdraw: Withdraw,
    cleanup: Cleanup,
    redraw: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Shelf {
    /// Binds a shelf to a shell and the roster its pages act on.
    #[must_use]
    pub fn new(view: &Rc<View>, roster: &Shared, surfaces: Surfaces) -> Rc<Self> {
        Self::with_cleanup(
            view,
            roster,
            surfaces,
            Rc::new(|_| {}),
            Rc::new(|_| {
                let (sent, received) = std::sync::mpsc::channel();
                let _ = sent.send(Ok(()));
                received
            }),
        )
    }

    #[must_use]
    pub fn with_lifecycle(view: &Rc<View>, roster: &Shared, surfaces: Surfaces, withdraw: Withdraw) -> Rc<Self> {
        Self::with_cleanup(
            view,
            roster,
            surfaces,
            withdraw,
            Rc::new(|_| {
                let (sent, received) = std::sync::mpsc::channel();
                let _ = sent.send(Ok(()));
                received
            }),
        )
    }

    #[must_use]
    pub fn with_cleanup(
        view: &Rc<View>,
        roster: &Shared,
        surfaces: Surfaces,
        withdraw: Withdraw,
        cleanup: Cleanup,
    ) -> Rc<Self> {
        let pages = gtk::Stack::new();
        pages.set_hexpand(true);
        pages.set_vexpand(true);
        let catalogue = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pages.add_named(&catalogue, Some(CATALOGUE));
        Rc::new(Self {
            view: Rc::downgrade(view),
            roster: Rc::clone(roster),
            surfaces,
            pages,
            catalogue,
            withdraw,
            cleanup,
            redraw: RefCell::new(None),
        })
    }

    /// Stops the extension and detaches its page while its durable record is
    /// retained for cleanup authority and recovery. The central lifecycle card
    /// remains alive to show progress or failure.
    pub fn quiesce(&self, name: &ExtensionName) -> Result<Entry, hl::extension::Refusal> {
        self.roster.borrow_mut().disable(name)?;
        let entry = self
            .roster
            .borrow()
            .entries()
            .into_iter()
            .find(|entry| entry.name == *name)
            .expect("a successfully disabled extension remains installed");
        (self.withdraw)(name);
        self.remove_surface(name);
        Ok(entry)
    }

    pub fn cleanup(&self, entry: Entry) -> Receiver<Result<(), String>> {
        (self.cleanup)(entry)
    }

    /// Puts a page and a settings page on the shell for every extension.
    pub fn install(self: &Rc<Self>) {
        for entry in self.roster.borrow().entries() {
            self.mount(&entry);
        }
        self.show_catalogue();
    }

    /// Puts one extension on the shell, replacing whatever was there under its
    /// name so a change of state rebuilds its host rather than talking to the
    /// one started under the old state.
    pub fn mount(self: &Rc<Self>, entry: &Entry) {
        self.unmount(&entry.name);
        if entry.stage == Stage::Duty {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let back = gtk::Button::with_label("Back to Extensions");
            back.set_halign(gtk::Align::Start);
            let shelf = Rc::downgrade(self);
            back.connect_clicked(move |_| {
                if let Some(shelf) = shelf.upgrade() {
                    shelf.show_catalogue();
                }
            });
            page.append(&back);
            page.append(&(self.surfaces)(entry));
            self.pages.add_named(&page, Some(entry.name.as_str()));
        }
        self.redraw();
    }

    /// Takes one extension off the shell, which is what drops its host.
    pub fn unmount(&self, name: &ExtensionName) {
        (self.withdraw)(name);
        self.remove_surface(name);
        self.show_catalogue();
        self.redraw();
    }

    /// Rebuilds one extension's pages from what the roster now says, and shows
    /// its settings page so an action a person took stays in front of them.
    pub fn refresh(self: &Rc<Self>, name: &ExtensionName) {
        let entry = self
            .roster
            .borrow()
            .entries()
            .into_iter()
            .find(|entry| entry.name == *name);
        let Some(entry) = entry else {
            self.unmount(name);
            return;
        };
        self.mount(&entry);
        if let Some(view) = self.view() {
            view.select_name(super::Page::Extensions.title());
        }
        self.redraw();
    }

    /// The roster every page on this shelf acts on.
    #[must_use]
    pub fn roster(&self) -> &Shared {
        &self.roster
    }

    /// Records a crash loop reported by the live host, then redraws central
    /// Settings. This runs on the GTK tick, never on the host thread.
    pub fn fault(self: &Rc<Self>, name: &ExtensionName, restarts: u32) {
        if let Err(refusal) = self.roster.borrow_mut().fault(name, restarts) {
            hl_log::hl_error!(hl_log::tag::RUNTIME, "recording extension fault for {name}: {refusal}");
            // Even when storage is unavailable, do not leave a dead sidecar's
            // provider or semantic callbacks advertised as live authority.
            self.unmount(name);
            return;
        }
        // Replacing the mounted Duty surface withdraws its provider generation
        // synchronously. Retry will mount a fresh generation whose providers
        // remain private until its first accepted frame.
        self.refresh(name);
    }

    /// The shell these pages sit on, while it is still open.
    #[must_use]
    pub fn view(&self) -> Option<Rc<View>> {
        self.view.upgrade()
    }

    pub fn content(&self) -> &gtk::Stack {
        &self.pages
    }

    pub fn catalogue(&self) -> &gtk::Box {
        &self.catalogue
    }

    pub fn open(&self, name: &ExtensionName) -> bool {
        if self.pages.child_by_name(name.as_str()).is_none() {
            self.show_catalogue();
            return false;
        }
        if let Some(view) = self.view() {
            view.select_name(super::Page::Extensions.title());
        }
        self.pages.set_visible_child_name(name.as_str());
        true
    }

    pub fn show_catalogue(&self) {
        self.pages.set_visible_child_name(CATALOGUE);
        if let Some(view) = self.view() {
            view.select_name(super::Page::Extensions.title());
        }
    }

    fn remove_surface(&self, name: &ExtensionName) {
        if let Some(surface) = self.pages.child_by_name(name.as_str()) {
            self.pages.remove(&surface);
        }
    }

    pub fn redraw_with(&self, redraw: Rc<dyn Fn()>) {
        self.redraw.replace(Some(redraw));
    }

    fn redraw(&self) {
        if let Some(redraw) = self.redraw.borrow().as_ref() {
            redraw();
        }
    }
}

impl std::fmt::Debug for Shelf {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Shelf")
            .field("installed", &self.roster.borrow().entries().len())
            .finish_non_exhaustive()
    }
}

/// Now, in milliseconds since the epoch, which is the moment a record is
/// written with. The screens have no clock of their own for the same reason
/// the policy has none: it is passed in and can be looked at.
#[must_use]
pub fn moment() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
