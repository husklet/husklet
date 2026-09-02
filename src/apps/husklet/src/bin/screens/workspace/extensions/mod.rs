//! Every extension a workspace has, as pages on the workspace shell.
//!
//! One extension gets two pages: the interface it draws itself, which is
//! [`super::extension::Interface`] over a host, and a settings page this
//! product writes. The settings page is ours on purpose — an extension must not
//! be the thing that offers to remove itself — and every action on it goes
//! through [`Roster`], which is where the lifecycle policy already lives.
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
pub type Surfaces = Rc<dyn Fn(&ExtensionName) -> gtk::Widget>;

/// How an image reference is read into something a person can be asked about.
///
/// The answer arrives on a channel because reading a manifest means reaching a
/// container daemon, and the main loop must keep drawing while that happens.
pub type Inspection = Rc<dyn Fn(&str) -> Receiver<Acquisition>>;

/// The word that separates an extension's name from the page of ours that is
/// about it. Used to build the sidebar label and nothing else.
const SETTINGS: &str = "settings";

/// Where a workspace's extensions live on the shell.
pub struct Shelf {
    /// Held weakly: the pages this shelf builds live inside the shell, and
    /// their actions hold the shelf, so a strong reference here would be a
    /// cycle that outlives the window.
    view: std::rc::Weak<View>,
    roster: Shared,
    surfaces: Surfaces,
}

impl Shelf {
    /// Binds a shelf to a shell and the roster its pages act on.
    #[must_use]
    pub fn new(view: &Rc<View>, roster: &Shared, surfaces: Surfaces) -> Rc<Self> {
        Rc::new(Self {
            view: Rc::downgrade(view),
            roster: Rc::clone(roster),
            surfaces,
        })
    }

    /// Puts a page and a settings page on the shell for every extension.
    pub fn install(self: &Rc<Self>) {
        for entry in self.roster.borrow().entries() {
            self.mount(&entry);
        }
    }

    /// Puts one extension on the shell, replacing whatever was there under its
    /// name so a change of state rebuilds its host rather than talking to the
    /// one started under the old state.
    pub fn mount(self: &Rc<Self>, entry: &Entry) {
        let Some(view) = self.view() else {
            return;
        };
        self.unmount(&entry.name);
        view.attach(&entry.name.to_string(), &(self.surfaces)(&entry.name));
        let page = settings::Settings::page(self, entry);
        view.attach(&settings_title(&entry.name), &page);
    }

    /// Takes one extension off the shell, which is what drops its host.
    pub fn unmount(&self, name: &ExtensionName) {
        let Some(view) = self.view() else {
            return;
        };
        view.detach(&name.to_string());
        view.detach(&settings_title(name));
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
            view.select_name(&settings_title(name));
        }
    }

    /// The roster every page on this shelf acts on.
    #[must_use]
    pub fn roster(&self) -> &Shared {
        &self.roster
    }

    /// The shell these pages sit on, while it is still open.
    #[must_use]
    pub fn view(&self) -> Option<Rc<View>> {
        self.view.upgrade()
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

/// The sidebar label of the settings page belonging to one extension.
#[must_use]
pub fn settings_title(name: &ExtensionName) -> String {
    format!("{name} {SETTINGS}")
}

/// Now, in milliseconds since the epoch, which is the moment a record is
/// written with. The screens have no clock of their own for the same reason
/// the policy has none: it is passed in and can be looked at.
#[must_use]
pub fn moment() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
