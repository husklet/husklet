//! Extension-owned pages mounted directly into the workspace overview shell.

mod console;
mod gallery;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use hl::extension::{Entry, Roster};
use hl_extension::{ExtensionName, Stage};
use hl_ws::storage::Directory;
use hl::config::WorkspaceConfig;

pub(crate) use console::Console;
pub use gallery::Gallery;

use super::View;

pub type Shared = Rc<RefCell<Roster<Directory>>>;
pub type Surfaces = Rc<dyn Fn(&Entry) -> gtk::Widget>;
pub type Withdraw = Rc<dyn Fn(&ExtensionName)>;

/// Keeps installed extension surfaces synchronized with the overview shell.
pub struct Shelf {
    view: std::rc::Weak<View>,
    roster: Shared,
    surfaces: Surfaces,
    withdraw: Withdraw,
    workspace: WorkspaceConfig,
    revision: Cell<u64>,
}

impl Shelf {
    #[must_use]
    pub fn new(view: &Rc<View>, workspace: &WorkspaceConfig, roster: &Shared, surfaces: Surfaces) -> Rc<Self> {
        Self::with_lifecycle(view, workspace, roster, surfaces, Rc::new(|_| {}))
    }

    #[must_use]
    pub fn with_lifecycle(
        view: &Rc<View>,
        workspace: &WorkspaceConfig,
        roster: &Shared,
        surfaces: Surfaces,
        withdraw: Withdraw,
    ) -> Rc<Self> {
        Rc::new(Self {
            view: Rc::downgrade(view),
            roster: Rc::clone(roster),
            surfaces,
            withdraw,
            workspace: workspace.clone(),
            revision: Cell::new(hl::extension::inventory_revision(workspace)),
        })
    }

    /// Mounts enabled pages in product order: Workspace, Extensions, then name.
    pub fn install(self: &Rc<Self>) {
        let mut entries = self.roster.borrow().entries();
        entries.sort_by(|left, right| order(left).cmp(&order(right)));
        for entry in entries {
            self.mount(&entry);
        }
    }

    pub fn mount(self: &Rc<Self>, entry: &Entry) {
        self.unmount(&entry.name);
        if entry.stage != Stage::Duty {
            return;
        }
        let Some(view) = self.view() else { return };
        let title = entry
            .interface
            .as_ref()
            .map_or(entry.display_name.as_str(), |presentation| {
                presentation.tab_title.as_str()
            });
        view.attach(entry.name.as_str(), title, &(self.surfaces)(entry));
    }

    pub fn unmount(&self, name: &ExtensionName) {
        (self.withdraw)(name);
        if let Some(view) = self.view() {
            view.detach(name.as_str());
        }
    }

    pub fn refresh(self: &Rc<Self>, name: &ExtensionName) {
        let entry = self
            .roster
            .borrow()
            .entries()
            .into_iter()
            .find(|entry| entry.name == *name);
        match entry {
            Some(entry) => self.mount(&entry),
            None => self.unmount(name),
        }
    }

    /// Reopens durable state only after a lifecycle mutation invalidated it.
    pub fn reconcile(self: &Rc<Self>) {
        let revision = hl::extension::inventory_revision(&self.workspace);
        if revision == self.revision.get() {
            return;
        }
        let Ok(roster) = Roster::workspace(&self.workspace) else {
            return;
        };
        let previous = self.roster.borrow().entries();
        let current = roster.entries();
        self.roster.replace(roster);
        for entry in &previous {
            if !current.iter().any(|candidate| candidate.name == entry.name) {
                self.unmount(&entry.name);
            }
        }
        for entry in current {
            let changed = previous.iter().find(|candidate| candidate.name == entry.name) != Some(&entry);
            if changed {
                self.mount(&entry);
            }
        }
        self.revision.set(revision);
    }

    pub fn fault(self: &Rc<Self>, name: &ExtensionName, restarts: u32) {
        if let Err(refusal) = self.roster.borrow_mut().fault(name, restarts) {
            hl_log::hl_error!(hl_log::tag::RUNTIME, "recording extension fault for {name}: {refusal}");
        }
        self.unmount(name);
    }

    #[must_use]
    pub fn roster(&self) -> &Shared {
        &self.roster
    }

    #[must_use]
    pub fn view(&self) -> Option<Rc<View>> {
        self.view.upgrade()
    }
}

fn order(entry: &Entry) -> (u8, &str) {
    let rank = match entry.name.as_str() {
        "workspace" => 0,
        "extensions" => 1,
        _ => 2,
    };
    (rank, entry.name.as_str())
}

impl std::fmt::Debug for Shelf {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Shelf")
            .field("installed", &self.roster.borrow().entries().len())
            .finish_non_exhaustive()
    }
}
