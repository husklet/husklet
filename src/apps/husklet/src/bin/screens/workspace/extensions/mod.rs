//! Every extension a workspace has, as pages on the workspace shell.
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

use hl::extension::{Acquisition, Entry, Roster};
use hl_extension::ExtensionName;
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
pub type Inspection = Rc<dyn Fn(&str) -> Receiver<Acquisition>>;

/// The bundled extension replacing Husklet's legacy operational pages.
///
/// This is the canonical identity of the base workspace-management image.
/// The older `containers` reference extension deliberately does not suppress
/// pages it cannot replace.
pub const MANAGEMENT_EXTENSION: &str = "workspace-manager";

/// Reconciles native operational fallback pages. `true` means the management
/// extension owns them and native duplicates must be absent.
pub type Reconcile = Rc<dyn Fn(bool)>;

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
    reconcile: Reconcile,
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
            Rc::new(|_| {}),
            Rc::new(|_| {
                let (sent, received) = std::sync::mpsc::channel();
                let _ = sent.send(Ok(()));
                received
            }),
        )
    }

    #[must_use]
    pub fn with_reconciliation(view: &Rc<View>, roster: &Shared, surfaces: Surfaces, reconcile: Reconcile) -> Rc<Self> {
        Self::with_lifecycle(view, roster, surfaces, reconcile, Rc::new(|_| {}))
    }

    #[must_use]
    pub fn with_lifecycle(
        view: &Rc<View>,
        roster: &Shared,
        surfaces: Surfaces,
        reconcile: Reconcile,
        withdraw: Withdraw,
    ) -> Rc<Self> {
        Self::with_cleanup(
            view,
            roster,
            surfaces,
            reconcile,
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
        reconcile: Reconcile,
        withdraw: Withdraw,
        cleanup: Cleanup,
    ) -> Rc<Self> {
        Rc::new(Self {
            view: Rc::downgrade(view),
            roster: Rc::clone(roster),
            surfaces,
            reconcile,
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
        if let Some(view) = self.view() {
            view.detach(&name.to_string());
        }
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
        self.reconcile_fallback();
    }

    /// Puts one extension on the shell, replacing whatever was there under its
    /// name so a change of state rebuilds its host rather than talking to the
    /// one started under the old state.
    pub fn mount(self: &Rc<Self>, entry: &Entry) {
        let Some(view) = self.view() else {
            return;
        };
        self.unmount(&entry.name);
        view.attach(&entry.name.to_string(), &(self.surfaces)(entry));
        self.reconcile_fallback();
        self.redraw();
    }

    /// Takes one extension off the shell, which is what drops its host.
    pub fn unmount(&self, name: &ExtensionName) {
        (self.withdraw)(name);
        let Some(view) = self.view() else {
            return;
        };
        view.detach(&name.to_string());
        self.reconcile_fallback();
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
    pub fn fault(&self, name: &ExtensionName, restarts: u32) {
        if let Err(refusal) = self.roster.borrow_mut().fault(name, restarts) {
            hl_log::hl_error!(hl_log::tag::RUNTIME, "recording extension fault for {name}: {refusal}");
        }
        self.redraw();
    }

    /// The shell these pages sit on, while it is still open.
    #[must_use]
    pub fn view(&self) -> Option<Rc<View>> {
        self.view.upgrade()
    }

    pub fn reconcile_fallback(&self) {
        let managed = self
            .roster
            .borrow()
            .entries()
            .iter()
            .any(|entry| entry.name.as_str() == MANAGEMENT_EXTENSION);
        (self.reconcile)(managed);
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
