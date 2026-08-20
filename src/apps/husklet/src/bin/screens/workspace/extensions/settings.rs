//! The page this product writes about one extension.
//!
//! Deliberately not the extension's own interface: what it is, what it was
//! granted, where it stands, and the four things a person may do to it. An
//! extension cannot draw this page, because an extension offering to remove
//! itself is not an offer anyone should have to trust.

use std::rc::Rc;

use gtk::prelude::*;
use hl::extension::{Entry, Refusal};
use hl_extension::{ExtensionName, Stage, Summary};

use super::Shelf;

/// Style class on the action that puts an extension on duty.
pub const ENABLE: &str = "hl-extension-enable";
/// Style class on the action that takes an extension off duty.
pub const DISABLE: &str = "hl-extension-disable";
/// Style class on the action that forgets an extension and its grant.
pub const REMOVE: &str = "hl-extension-remove";
/// Style class on the action offered only to a faulted extension.
pub const RETRY: &str = "hl-extension-retry-fault";
/// Style class on the line saying where the extension stands.
pub const STANDING: &str = "hl-extension-standing";
/// Style class on the line saying why an action was refused.
pub const REFUSAL: &str = "hl-extension-refusal";

/// One extension's settings page.
pub struct Settings;

impl Settings {
    /// Builds the page for one extension as the roster currently describes it.
    #[must_use]
    pub fn page(shelf: &Rc<Shelf>, entry: &Entry) -> gtk::ScrolledWindow {
        let main = gtk::Box::new(gtk::Orientation::Vertical, 12);
        main.add_css_class("dmain");
        main.append(&heading(&entry.name));
        main.append(&line(&format!("image  ·  {}", entry.image_digest), "fhint"));
        main.append(&standing(entry.stage));
        main.append(&capabilities(entry));
        let refusal = line("", REFUSAL);
        refusal.set_visible(false);
        main.append(&actions(shelf, entry, &refusal));
        main.append(&refusal);
        gtk::ScrolledWindow::builder()
            .child(&main)
            .hexpand(true)
            .vexpand(true)
            .build()
    }
}

/// The extension's own name, which is also what its pages are labelled with.
fn heading(name: &ExtensionName) -> gtk::Label {
    let label = gtk::Label::new(Some(&name.to_string()));
    label.add_css_class("dhead");
    label.set_xalign(0.0);
    label
}

/// Where the extension stands, in the words a person needs.
///
/// A fault is spelled out with its restart count rather than shown as "off",
/// because a person who never chose it would otherwise find it stopped with no
/// explanation.
fn standing(stage: Stage) -> gtk::Label {
    let said = match stage {
        Stage::Vacancy => "not installed".to_owned(),
        Stage::Standby => "disabled".to_owned(),
        Stage::Duty => "enabled".to_owned(),
        Stage::Fault { restarts } => format!("faulted after {restarts} restarts"),
    };
    line(&said, STANDING)
}

/// What the person agreed to, and the sentence a grant that runs code owes them.
fn capabilities(entry: &Entry) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let summary = Summary::of(&entry.granted);
    if summary.execution {
        column.append(&line(Summary::EXECUTION_NOTICE, "fhint"));
    }
    if entry.granted.is_empty() {
        column.append(&line("granted nothing", "fhint"));
        return column;
    }
    for capability in entry.granted.iter() {
        column.append(&line(capability.as_str(), "fhint"));
    }
    column
}

/// The actions the current stage allows.
fn actions(shelf: &Rc<Shelf>, entry: &Entry, refusal: &gtk::Label) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    if entry.stage.is_fault() {
        row.append(&action(shelf, entry, refusal, "Retry", RETRY, Deed::Retry));
    }
    if entry.stage == Stage::Duty {
        row.append(&action(shelf, entry, refusal, "Disable", DISABLE, Deed::Disable));
    } else {
        row.append(&action(shelf, entry, refusal, "Enable", ENABLE, Deed::Enable));
    }
    row.append(&action(shelf, entry, refusal, "Remove", REMOVE, Deed::Remove));
    row
}

/// What one button does to the roster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Deed {
    Enable,
    Disable,
    Retry,
    Remove,
}

/// One action, wired to the roster and to the shelf that redraws after it.
fn action(shelf: &Rc<Shelf>, entry: &Entry, refusal: &gtk::Label, label: &str, class: &str, deed: Deed) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class(class);
    let shelf = Rc::clone(shelf);
    let name = entry.name.clone();
    let refusal = refusal.clone();
    button.connect_clicked(move |_| commit(&shelf, &name, deed, &refusal));
    button
}

/// Puts one action through the policy and redraws what it produced.
///
/// A refusal is shown on the page rather than logged, because the person is
/// standing in front of the thing they just asked for.
fn commit(shelf: &Rc<Shelf>, name: &ExtensionName, deed: Deed, refusal: &gtk::Label) {
    let done = apply(shelf, name, deed);
    if let Err(fault) = done {
        refusal.set_text(&fault.to_string());
        refusal.set_visible(true);
        return;
    }
    shelf.refresh(name);
}

/// The roster call one deed stands for, with the borrow released before the
/// shelf redraws from the same roster.
fn apply(shelf: &Rc<Shelf>, name: &ExtensionName, deed: Deed) -> Result<(), Refusal> {
    let mut roster = shelf.roster().borrow_mut();
    match deed {
        Deed::Enable => roster.enable(name),
        Deed::Disable => roster.disable(name),
        Deed::Retry => roster.retry(name),
        Deed::Remove => roster.remove(name),
    }
}

/// One line of text on the page.
fn line(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}
