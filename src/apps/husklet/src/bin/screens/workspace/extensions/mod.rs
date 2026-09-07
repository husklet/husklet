//! Extension-owned pages mounted directly into the workspace overview shell.

mod console;
mod gallery;

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use hl::config::WorkspaceConfig;
use hl::extension::{Entry, Roster};
use hl_extension::{ExtensionName, Stage};
use hl_ws::storage::Directory;

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

    /// Mounts enabled pages in product order: Top, then third-party names.
    pub fn install(self: &Rc<Self>) {
        let mut entries = self.roster.borrow().entries();
        entries.sort_by(|left, right| order(left).cmp(&order(right)));
        for entry in entries {
            self.mount(&entry);
        }
    }

    pub fn mount(self: &Rc<Self>, entry: &Entry) {
        self.unmount(&entry.name);
        let Some(view) = self.view() else { return };
        let title = entry
            .interface
            .as_ref()
            .map_or(entry.display_name.as_str(), |presentation| {
                presentation.tab_title.as_str()
            });
        let surface = match entry.stage {
            Stage::Duty => (self.surfaces)(entry),
            Stage::Fault { restarts } => self.fault_surface(entry, restarts),
            Stage::Standby | Stage::Vacancy => return,
        };
        view.attach(entry.name.as_str(), title, &surface);
    }

    /// Keeps a crash-looping extension visible and gives the person one clear
    /// recovery action. A durable fault must not turn an installed extension
    /// into an empty sidebar: that makes failure look exactly like uninstall.
    fn fault_surface(self: &Rc<Self>, entry: &Entry, restarts: u32) -> gtk::Widget {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.set_halign(gtk::Align::Center);
        page.set_valign(gtk::Align::Center);
        page.set_margin_start(24);
        page.set_margin_end(24);

        let title = gtk::Label::new(Some(&format!("{} needs attention", entry.display_name)));
        title.add_css_class("title-2");
        let detail = gtk::Label::new(Some(&format!(
            "The extension stopped {restarts} times, so Husklet paused it to protect this workspace."
        )));
        detail.set_wrap(true);
        detail.set_justify(gtk::Justification::Center);
        detail.add_css_class("dim-label");
        let retry = gtk::Button::with_label("Retry extension");
        retry.add_css_class("suggested-action");
        retry.set_halign(gtk::Align::Center);

        let name = entry.name.clone();
        let weak = Rc::downgrade(self);
        retry.connect_clicked(move |button| {
            button.set_sensitive(false);
            let Some(shelf) = weak.upgrade() else { return };
            if let Err(refusal) = shelf.roster.borrow_mut().retry(&name) {
                hl_log::hl_error!(hl_log::tag::RUNTIME, "retrying extension {name}: {refusal}");
                button.set_sensitive(true);
                return;
            }
            shelf.refresh(&name);
        });

        page.append(&title);
        page.append(&detail);
        page.append(&retry);
        page.upcast()
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
        self.refresh(name);
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
        "top" => 0,
        _ => 1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use hl_extension::{Activation, Grant, Manifest, Resources};

    fn manifest() -> Manifest {
        Manifest {
            name: ExtensionName::new("top").expect("name"),
            display_name: "Top".to_owned(),
            version: "1.0.0".to_owned(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::default(),
            entrypoint: None,
            activation: Activation::Tab,
            interface: None,
            pane_providers: Vec::new(),
            resources: Resources::default(),
            filesystem_roots: Vec::new(),
        }
    }

    #[test]
    fn a_faulted_extension_stays_visible_and_retry_starts_a_fresh_surface() {
        if !crate::test_support::on_the_toolkit_thread(|| {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
            workspace.storage = Some(temporary.path().join("workspace"));
            let mut roster = Roster::workspace(&workspace).expect("roster");
            let manifest = manifest();
            roster
                .register(&manifest, "sha256:top", &Grant::default(), 1)
                .expect("record");
            roster.enable(&manifest.name).expect("enabled");
            roster.fault(&manifest.name, 5).expect("faulted");

            let roster = Rc::new(RefCell::new(roster));
            let view = Rc::new(View::with_semantics(
                [],
                super::super::semantic::Registry::new("workspace"),
            ));
            let starts = Rc::new(Cell::new(0));
            let counted = Rc::clone(&starts);
            let surfaces: Surfaces = Rc::new(move |_| {
                counted.set(counted.get() + 1);
                gtk::Label::new(Some("running")).upcast()
            });
            let shelf = Shelf::new(&view, &workspace, &roster, surfaces);
            shelf.install();

            assert_eq!(view.entries(), ["Top"]);
            assert_eq!(starts.get(), 0, "a faulted sidecar must stay stopped");
            let page = view
                .page("top")
                .expect("fault page")
                .downcast::<gtk::Box>()
                .expect("fault page box");
            let retry = page
                .last_child()
                .expect("retry")
                .downcast::<gtk::Button>()
                .expect("retry button");
            assert_eq!(retry.label().as_deref(), Some("Retry extension"));

            retry.emit_clicked();
            assert_eq!(starts.get(), 1, "retry mounts exactly one fresh host surface");
            assert_eq!(roster.borrow().stage(&manifest.name), Stage::Duty);
        }) {
            eprintln!("skipped: no display connection");
        }
    }
}
