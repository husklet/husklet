//! The lifecycle card this product writes about one extension.
//!
//! Deliberately not the extension's own interface: what it is, what it was
//! granted, where it stands, and the four things a person may do to it. An
//! extension cannot draw this page, because an extension offering to remove
//! itself is not an offer anyone should have to trust. Cards live together on
//! the central Extensions page; they are not extra sidebar destinations.

use std::rc::Rc;
use std::sync::mpsc::TryRecvError;

use gtk::prelude::*;
use hl::extension::{Entry, Refusal};
use hl_extension::{ExtensionName, Stage, Summary};

use super::Shelf;

/// Style class on the action that puts an extension on duty.
pub const ENABLE: &str = "hl-extension-enable";
/// Style class on the action that takes an extension off duty.
pub const DISABLE: &str = "hl-extension-disable";
/// Style class on the action that begins a reviewed image update.
pub const UPDATE: &str = "hl-extension-update";
/// Style class on the action that forgets an extension and its grant.
pub const REMOVE: &str = "hl-extension-remove";
pub const CONFIRM_REMOVE: &str = "hl-extension-confirm-remove";
pub const CANCEL_REMOVE: &str = "hl-extension-cancel-remove";
/// Style class on the action offered only to a faulted extension.
pub const RETRY: &str = "hl-extension-retry-fault";
/// Style class on the line saying where the extension stands.
pub const STANDING: &str = "hl-extension-standing";
/// Style class on the line saying why an action was refused.
pub const REFUSAL: &str = "hl-extension-refusal";
/// Style class identifying one lifecycle card in the central catalogue.
pub const CARD: &str = "hl-extension-card";
/// Wrapping action region inside a lifecycle card.
pub const ACTIONS: &str = "hl-extension-actions";

/// One extension's lifecycle card.
pub struct Settings;

impl Settings {
    /// Builds the page for one extension as the roster currently describes it.
    #[must_use]
    pub fn page(
        shelf: &Rc<Shelf>,
        entry: &Entry,
        semantics: &super::super::semantic::Registry,
        update: Rc<dyn Fn()>,
    ) -> gtk::Box {
        let main = gtk::Box::new(gtk::Orientation::Vertical, 12);
        main.add_css_class("dmain");
        main.add_css_class(CARD);
        main.append(&heading(&entry.name));
        main.append(&line(
            &format!(
                "version  ·  {}",
                if entry.version.is_empty() {
                    "unknown"
                } else {
                    &entry.version
                }
            ),
            "fhint",
        ));
        main.append(&line(&format!("image  ·  {}", entry.image_digest), "fhint"));
        let standing = standing(entry.stage);
        main.append(&standing);
        main.append(&capabilities(entry));
        let refusal = line("", REFUSAL);
        refusal.set_visible(false);
        main.append(&actions(shelf, entry, &refusal, &standing, semantics, update));
        main.append(&refusal);
        let prefix = format!("extensions/installed/{}/", entry.name);
        let version = if entry.version.is_empty() {
            "unknown"
        } else {
            &entry.version
        };
        let card = format!("version {version}; {}", standing.text());
        semantics.register(
            &format!("{prefix}card"),
            "group",
            Some(entry.name.as_str()),
            Some(super::super::semantic::Value::Public(&card)),
            &[],
            Rc::new(|_, _| {}),
        );
        let capabilities = if entry.granted.is_empty() {
            "none".to_owned()
        } else {
            entry
                .granted
                .iter()
                .map(hl_extension::Capability::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        semantics.register(
            &format!("{prefix}capabilities"),
            "list",
            Some("Granted capabilities"),
            Some(super::super::semantic::Value::Public(&capabilities)),
            &[],
            Rc::new(|_, _| {}),
        );
        semantics.register(
            &format!("{prefix}status"),
            "status",
            Some(entry.name.as_str()),
            Some(super::super::semantic::Value::Public(standing.text().as_str())),
            &[],
            Rc::new(|_, _| {}),
        );
        semantics.register(
            &format!("{prefix}digest"),
            "text",
            Some("Image digest"),
            Some(super::super::semantic::Value::Public(&entry.image_digest)),
            &[],
            Rc::new(|_, _| {}),
        );
        semantics.register(
            &format!("{prefix}notice"),
            "status",
            Some("Lifecycle notice"),
            Some(super::super::semantic::Value::Public("")),
            &[],
            Rc::new(|_, _| {}),
        );
        main
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
        Stage::Fault { restarts } => {
            format!("enabled, but stopped after {restarts} failed starts; retry or disable it")
        }
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
fn actions(
    shelf: &Rc<Shelf>,
    entry: &Entry,
    refusal: &gtk::Label,
    standing: &gtk::Label,
    semantics: &super::super::semantic::Registry,
    update: Rc<dyn Fn()>,
) -> gtk::FlowBox {
    let row = gtk::FlowBox::new();
    row.add_css_class(ACTIONS);
    row.set_selection_mode(gtk::SelectionMode::None);
    row.set_min_children_per_line(1);
    row.set_max_children_per_line(3);
    row.set_column_spacing(8);
    row.set_row_spacing(8);
    if entry.stage.is_fault() {
        row.insert(
            &action(shelf, entry, refusal, "Retry", RETRY, Deed::Retry, semantics),
            -1,
        );
    }
    if entry.stage == Stage::Duty || entry.stage.is_fault() {
        row.insert(
            &action(shelf, entry, refusal, "Disable", DISABLE, Deed::Disable, semantics),
            -1,
        );
    } else {
        row.insert(
            &action(shelf, entry, refusal, "Enable", ENABLE, Deed::Enable, semantics),
            -1,
        );
    }
    row.insert(&update_action(entry, semantics, update), -1);
    row.insert(&removal(shelf, entry, refusal, standing, semantics), -1);
    row
}

/// Guides the user into the existing digest- and grant-reviewed update flow.
fn update_action(entry: &Entry, semantics: &super::super::semantic::Registry, update: Rc<dyn Fn()>) -> gtk::Button {
    use super::super::semantic::ActionKind;
    let button = gtk::Button::with_label("Update");
    button.add_css_class(UPDATE);
    let clicked = Rc::clone(&update);
    button.connect_clicked(move |_| clicked());
    let focused = button.clone();
    semantics.register(
        &format!("extensions/installed/{}/Update", entry.name),
        "button",
        Some("Update"),
        Some(super::super::semantic::Value::Public(
            "Choose a newer image, then review its digest and capability changes",
        )),
        &[ActionKind::Invoke, ActionKind::Focus],
        Rc::new(move |action, _| match action {
            ActionKind::Invoke => update(),
            ActionKind::Focus => {
                focused.grab_focus();
            }
            _ => {}
        }),
    );
    button
}

/// What one button does to the roster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Deed {
    Enable,
    Disable,
    Retry,
}

/// One action, wired to the roster and to the shelf that redraws after it.
fn action(
    shelf: &Rc<Shelf>,
    entry: &Entry,
    refusal: &gtk::Label,
    label: &str,
    class: &str,
    deed: Deed,
    semantics: &super::super::semantic::Registry,
) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class(class);
    let semantic_shelf = Rc::clone(shelf);
    let semantic_refusal = refusal.clone();
    let shelf = Rc::clone(shelf);
    let name = entry.name.clone();
    let image_digest = entry.image_digest.clone();
    let refusal = refusal.clone();
    button.connect_clicked(move |_| commit(&shelf, &name, &image_digest, deed, &refusal));
    let name = entry.name.clone();
    let image_digest = entry.image_digest.clone();
    let semantic_button = button.clone();
    semantics.register(
        &format!("extensions/installed/{}/{label}", entry.name),
        "button",
        Some(label),
        None,
        &[
            super::super::semantic::ActionKind::Invoke,
            super::super::semantic::ActionKind::Focus,
        ],
        Rc::new(move |action, _| match action {
            super::super::semantic::ActionKind::Invoke => {
                commit(&semantic_shelf, &name, &image_digest, deed, &semantic_refusal)
            }
            super::super::semantic::ActionKind::Focus => {
                semantic_button.grab_focus();
            }
            _ => {}
        }),
    );
    button
}

/// Puts one action through the policy and redraws what it produced.
///
/// A refusal is shown on the page rather than logged, because the person is
/// standing in front of the thing they just asked for.
fn commit(shelf: &Rc<Shelf>, name: &ExtensionName, image_digest: &str, deed: Deed, refusal: &gtk::Label) {
    let done = apply(shelf, name, image_digest, deed);
    if let Err(fault) = done {
        refusal.set_text(&fault.to_string());
        refusal.set_visible(true);
        return;
    }
    shelf.refresh(name);
}

/// The roster call one deed stands for, with the borrow released before the
/// shelf redraws from the same roster.
fn apply(shelf: &Rc<Shelf>, name: &ExtensionName, image_digest: &str, deed: Deed) -> Result<(), Refusal> {
    let mut roster = shelf.roster().borrow_mut();
    match deed {
        Deed::Enable => roster.enable_if_digest(name, image_digest),
        Deed::Disable => roster.disable_if_digest(name, image_digest),
        Deed::Retry => roster.retry_if_digest(name, image_digest),
    }
}

/// A destructive removal is a separate, confirmed transaction. Runtime
/// cleanup finishes first; only its success forgets the durable grant.
fn removal(
    shelf: &Rc<Shelf>,
    entry: &Entry,
    refusal: &gtk::Label,
    standing: &gtk::Label,
    semantics: &super::super::semantic::Registry,
) -> gtk::Box {
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class(REMOVE);
    let confirm = gtk::Button::with_label("Confirm removal");
    confirm.add_css_class(CONFIRM_REMOVE);
    confirm.set_visible(false);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class(CANCEL_REMOVE);
    cancel.set_visible(false);
    controls.append(&remove);
    controls.append(&confirm);
    controls.append(&cancel);
    for (label, button) in [
        ("Remove", &remove),
        ("Confirm removal", &confirm),
        ("Cancel removal", &cancel),
    ] {
        let button = button.clone();
        semantics.register(
            &format!("extensions/installed/{}/{label}", entry.name),
            "button",
            Some(label),
            None,
            &[
                super::super::semantic::ActionKind::Invoke,
                super::super::semantic::ActionKind::Focus,
            ],
            Rc::new(move |action, _| match action {
                super::super::semantic::ActionKind::Invoke => button.emit_clicked(),
                super::super::semantic::ActionKind::Focus => {
                    button.grab_focus();
                }
                _ => {}
            }),
        );
    }
    let confirm_path = format!("extensions/installed/{}/Confirm removal", entry.name);
    let status_path = format!("extensions/installed/{}/status", entry.name);
    let notice_path = format!("extensions/installed/{}/notice", entry.name);
    semantics.set_destructive(&confirm_path);
    semantics.set_disabled(&confirm_path, true);
    let remove_path = format!("extensions/installed/{}/Remove", entry.name);
    let cancel_path = format!("extensions/installed/{}/Cancel removal", entry.name);
    semantics.set_disabled(&cancel_path, true);

    {
        let remove = remove.clone();
        let confirm = confirm.clone();
        let cancel = cancel.clone();
        let refusal = refusal.clone();
        let semantics = semantics.clone();
        let confirm_path = confirm_path.clone();
        let remove_path = remove_path.clone();
        let cancel_path = cancel_path.clone();
        let notice_path = notice_path.clone();
        remove.clone().connect_clicked(move |_| {
            let prompt = "Remove this extension, its saved grant, and its managed sidecar?";
            refusal.set_text(prompt);
            refusal.set_visible(true);
            remove.set_visible(false);
            confirm.set_visible(true);
            cancel.set_visible(true);
            semantics.set_disabled(&remove_path, true);
            semantics.set_disabled(&confirm_path, false);
            semantics.set_disabled(&cancel_path, false);
            semantics.update(&notice_path, super::super::semantic::Value::Public(prompt), false);
        });
    }
    {
        let remove = remove.clone();
        let confirm = confirm.clone();
        let cancel = cancel.clone();
        let refusal = refusal.clone();
        let semantics = semantics.clone();
        let confirm_path = confirm_path.clone();
        let remove_path = remove_path.clone();
        let cancel_path = cancel_path.clone();
        let notice_path = notice_path.clone();
        cancel.clone().connect_clicked(move |_| {
            refusal.set_visible(false);
            remove.set_visible(true);
            confirm.set_visible(false);
            cancel.set_visible(false);
            semantics.set_disabled(&remove_path, false);
            semantics.set_disabled(&confirm_path, true);
            semantics.set_disabled(&cancel_path, true);
            semantics.update(
                &notice_path,
                super::super::semantic::Value::Public("Removal cancelled; nothing changed"),
                false,
            );
        });
    }
    {
        let shelf = Rc::clone(shelf);
        let name = entry.name.clone();
        let image_digest = entry.image_digest.clone();
        let confirm = confirm.clone();
        let cancel = cancel.clone();
        let refusal = refusal.clone();
        let standing = standing.clone();
        let semantics = semantics.clone();
        let confirm_path = confirm_path.clone();
        let cancel_path = cancel_path.clone();
        let status_path = status_path.clone();
        let notice_path = notice_path.clone();
        confirm.clone().connect_clicked(move |_| {
            semantics.set_disabled(&confirm_path, true);
            semantics.set_disabled(&cancel_path, true);
            let unchanged = shelf
                .roster()
                .borrow()
                .entries()
                .into_iter()
                .any(|entry| entry.name == name && entry.image_digest == image_digest);
            if !unchanged {
                refusal.set_text("The extension changed; inspect and confirm removal again.");
                refusal.set_visible(true);
                semantics.set_disabled(&cancel_path, false);
                return;
            }
            let entry = match shelf.quiesce(&name) {
                Ok(entry) => entry,
                Err(fault) => {
                    let failure = fault.to_string();
                    refusal.set_text(&failure);
                    refusal.set_visible(true);
                    confirm.set_label("Retry removal");
                    semantics.set_label(&confirm_path, "Retry removal");
                    semantics.set_disabled(&confirm_path, false);
                    semantics.set_disabled(&cancel_path, false);
                    semantics.update(
                        &notice_path,
                        super::super::semantic::Value::Public(&failure),
                        false,
                    );
                    return;
                }
            };
            standing.set_text("disabled · removing managed sidecar");
            let removing = "Removing the managed sidecar before forgetting this extension…";
            refusal.set_text(removing);
            refusal.set_visible(true);
            semantics.update(
                &status_path,
                super::super::semantic::Value::Public("disabled · removing managed sidecar"),
                false,
            );
            semantics.update(
                &notice_path,
                super::super::semantic::Value::Public(removing),
                false,
            );
            confirm.set_label("Removing…");
            confirm.set_sensitive(false);
            cancel.set_sensitive(false);
            let answer = shelf.cleanup(entry);
            let shelf = Rc::clone(&shelf);
            let name = name.clone();
            let confirm = confirm.clone();
            let cancel = cancel.clone();
            let refusal = refusal.clone();
            let standing = standing.clone();
            let semantics = semantics.clone();
            let confirm_path = confirm_path.clone();
            let cancel_path = cancel_path.clone();
            let status_path = status_path.clone();
            let notice_path = notice_path.clone();
            let confirmed_digest = image_digest.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(100), move || match answer.try_recv() {
                Ok(Ok(())) => {
                    let forgotten = shelf.roster().borrow_mut().remove_if_digest(&name, &confirmed_digest);
                    if let Err(fault) = forgotten {
                        let failure = format!(
                            "The managed sidecar was removed, but the installation record could not be forgotten: {fault}"
                        );
                        refusal.set_text(&failure);
                        refusal.set_visible(true);
                        standing.set_text("disabled · record cleanup failed");
                        confirm.set_label("Retry removal");
                        confirm.set_sensitive(true);
                        cancel.set_sensitive(true);
                        semantics.set_label(&confirm_path, "Retry removal");
                        semantics.set_disabled(&confirm_path, false);
                        semantics.set_disabled(&cancel_path, false);
                        semantics.update(
                            &status_path,
                            super::super::semantic::Value::Public("disabled · record cleanup failed"),
                            false,
                        );
                        semantics.update(
                            &notice_path,
                            super::super::semantic::Value::Public(&failure),
                            false,
                        );
                    } else {
                        shelf.refresh(&name);
                    }
                    gtk::glib::ControlFlow::Break
                }
                Ok(Err(reason)) => {
                    let failure = format!(
                        "Removal failed; the extension remains installed and disabled: {reason}"
                    );
                    refusal.set_text(&failure);
                    refusal.set_visible(true);
                    standing.set_text("disabled · removal failed");
                    confirm.set_label("Retry removal");
                    confirm.set_sensitive(true);
                    cancel.set_sensitive(true);
                    semantics.set_label(&confirm_path, "Retry removal");
                    semantics.set_disabled(&confirm_path, false);
                    semantics.set_disabled(&cancel_path, false);
                    semantics.update(
                        &status_path,
                        super::super::semantic::Value::Public("disabled · removal failed"),
                        false,
                    );
                    semantics.update(
                        &notice_path,
                        super::super::semantic::Value::Public(&failure),
                        false,
                    );
                    gtk::glib::ControlFlow::Break
                }
                Err(TryRecvError::Disconnected) => {
                    let failure = "Removal failed; the cleanup worker ended without an answer";
                    refusal.set_text(failure);
                    refusal.set_visible(true);
                    standing.set_text("disabled · removal failed");
                    confirm.set_label("Retry removal");
                    confirm.set_sensitive(true);
                    cancel.set_sensitive(true);
                    semantics.set_label(&confirm_path, "Retry removal");
                    semantics.set_disabled(&confirm_path, false);
                    semantics.set_disabled(&cancel_path, false);
                    semantics.update(
                        &status_path,
                        super::super::semantic::Value::Public("disabled · removal failed"),
                        false,
                    );
                    semantics.update(
                        &notice_path,
                        super::super::semantic::Value::Public(failure),
                        false,
                    );
                    gtk::glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            });
        });
    }
    controls
}

/// One line of text on the page.
fn line(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}
