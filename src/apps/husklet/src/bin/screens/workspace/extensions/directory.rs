//! The workspace page listing every extension, and the only way to add one.
//!
//! Adding an extension is two steps on purpose. The image is read first and
//! nothing is written; what it asks for is shown; and only an answer a person
//! gives records a grant. There is no path on this page from an image's request
//! to a recorded grant that skips the middle step.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;

use gtk::prelude::*;
use hl::extension::{Acquisition, Candidate};
use hl_extension::{Capability, Grant, Manifest, Summary, Update};

use super::{moment, Inspection, PendingInspection, Shelf};

/// Style class on the field an image reference is typed into.
pub const REFERENCE: &str = "hl-extension-reference";
/// Style class on the action that reads an image's manifest.
pub const INSPECT: &str = "hl-extension-inspect";
/// Style class on the action that records the grant.
pub const CONSENT: &str = "hl-extension-consent";
/// Style class on the action that walks away from a candidate.
pub const DECLINE: &str = "hl-extension-decline";
/// Style class on the line reporting what the page just did.
pub const NOTICE: &str = "hl-extension-notice";
/// Style class on the image acquisition progress view.
pub const PROGRESS: &str = "hl-extension-progress";
pub const CANCEL_ACQUISITION: &str = "hl-extension-cancel-acquisition";
/// Style class on the block describing a candidate image.
pub const PROPOSAL: &str = "hl-extension-proposal";
pub const PROPOSAL_CAPABILITIES: &str = "hl-extension-proposal-capabilities";
pub const UPDATE_DELTA: &str = "hl-extension-update-delta";
pub const UPDATE_CAPABILITIES: &str = "hl-extension-update-capabilities";
pub const CAPABILITY_CHOICE: &str = "hl-extension-capability-choice";

type Selection = Rc<RefCell<BTreeSet<Capability>>>;

#[derive(Clone)]
enum Proposal {
    Install {
        candidate: Candidate,
        selected: Selection,
    },
    Update {
        candidate: Candidate,
        update: Update,
        selected: Selection,
    },
}

/// How often the page looks for an inspection that has come back.
///
/// Matches the other live workspace pages, so reading one image's manifest does
/// not set the application's rhythm.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// The "Extensions" page: what is installed, and how to install another.
pub struct Catalogue {
    widget: gtk::Box,
    viewport: gtk::ScrolledWindow,
    shelf: Rc<Shelf>,
    inspection: Inspection,
    listing: gtk::Box,
    reference: gtk::Entry,
    inspect: gtk::Button,
    proposal: gtk::Box,
    notice: gtk::Label,
    progress: gtk::ProgressBar,
    cancel: gtk::Button,
    /// The inspection in flight, if any. One at a time, because the field it
    /// was started from is the same field a second one would read.
    pending: RefCell<Option<PendingInspection>>,
    /// What the last inspection found, waiting for an answer.
    candidate: RefCell<Option<Proposal>>,
    consent_focus: Cell<bool>,
    semantics: super::super::semantic::Registry,
}

impl Catalogue {
    #[cfg(feature = "native-test-hooks")]
    pub(crate) fn proposed_candidate(&self) -> Option<Candidate> {
        self.candidate.borrow().as_ref().map(|proposal| match proposal {
            Proposal::Install { candidate, .. } | Proposal::Update { candidate, .. } => candidate.clone(),
        })
    }

    /// Builds the page and puts its polling on the main loop.
    #[must_use]
    pub fn new(shelf: &Rc<Shelf>, inspection: Inspection) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 12);
        widget.add_css_class("dmain");
        let viewport = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&widget)
            .build();
        viewport.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let notice = text("", NOTICE);
        // Validation, acquisition, cancellation, consent, and retry all report
        // through this one changing line. Mark it as a live status so its new
        // value is announced without pulling focus away from the active field
        // or action.
        notice.set_accessible_role(gtk::AccessibleRole::Status);
        let proposal = gtk::Box::new(gtk::Orientation::Vertical, 6);
        proposal.add_css_class(PROPOSAL);
        proposal.set_visible(false);
        let progress = gtk::ProgressBar::new();
        progress.add_css_class(PROGRESS);
        progress.set_show_text(true);
        progress.set_visible(false);
        let cancel = gtk::Button::with_label("Cancel download");
        cancel.add_css_class(CANCEL_ACQUISITION);
        cancel.set_visible(false);
        let semantics = shelf.view().map_or_else(
            || super::super::semantic::Registry::new("workspace"),
            |view| view.semantic_registry(),
        );
        let page = Rc::new(Self {
            widget,
            viewport,
            shelf: Rc::clone(shelf),
            inspection,
            listing: gtk::Box::new(gtk::Orientation::Vertical, 4),
            reference: gtk::Entry::new(),
            inspect: gtk::Button::with_label("Read manifest"),
            proposal,
            notice,
            progress,
            cancel,
            pending: RefCell::new(None),
            candidate: RefCell::new(None),
            consent_focus: Cell::new(false),
            semantics,
        });
        page.assemble();
        let weak = Rc::downgrade(&page);
        shelf.redraw_with(Rc::new(move || {
            if let Some(page) = weak.upgrade() {
                page.refresh();
            }
        }));
        page.refresh();
        page
    }

    /// The page, for placing on the shell.
    #[must_use]
    pub const fn widget(&self) -> &gtk::Box {
        &self.widget
    }

    /// The bounded workspace-page viewport that keeps long catalogues usable
    /// without making the containing window adopt their full natural height.
    #[must_use]
    pub const fn viewport(&self) -> &gtk::ScrolledWindow {
        &self.viewport
    }

    #[must_use]
    pub const fn shelf(&self) -> &Rc<Shelf> {
        &self.shelf
    }

    /// Redraws the listing from what the roster now says.
    pub fn refresh(&self) {
        self.semantics.remove_prefix("extensions/installed/");
        while let Some(child) = self.listing.first_child() {
            self.listing.remove(&child);
        }
        let entries = self.shelf.roster().borrow().entries();
        if entries.is_empty() {
            self.listing.append(&text("— none installed —", "dhint"));
            self.semantics.register(
                "extensions/installed/empty",
                "status",
                Some("Installed extensions"),
                Some(super::super::semantic::Value::Public("None installed")),
                &[],
                Rc::new(|_, _| {}),
            );
            return;
        }
        for entry in entries {
            let reference = self.reference.clone();
            let notice = self.notice.clone();
            let semantics = self.semantics.clone();
            let name = entry.name.clone();
            let update = Rc::new(move || {
                reference.grab_focus();
                let message = format!(
                    "Enter a newer image reference for {name}, then read its manifest to review the digest and capability changes."
                );
                notice.set_text(&message);
                notice.set_visible(true);
                semantics.update(
                    "extensions/notice",
                    super::super::semantic::Value::Public(&message),
                    false,
                );
            });
            let card = super::settings::Settings::page(&self.shelf, &entry, &self.semantics, update);
            if entry.stage == hl_extension::Stage::Duty {
                use super::super::semantic::ActionKind;
                let open = gtk::Button::with_label("Open");
                open.set_halign(gtk::Align::Start);
                let shelf = Rc::clone(&self.shelf);
                let name = entry.name.clone();
                open.connect_clicked(move |_| {
                    shelf.open(&name);
                });
                let shelf = Rc::clone(&self.shelf);
                let name = entry.name.clone();
                let focused = open.clone();
                self.semantics.register(
                    &format!("extensions/installed/{}/Open", entry.name),
                    "button",
                    Some("Open"),
                    None,
                    &[ActionKind::Invoke, ActionKind::Focus],
                    Rc::new(move |action, _| match action {
                        ActionKind::Invoke => {
                            shelf.open(&name);
                        }
                        ActionKind::Focus => {
                            focused.grab_focus();
                        }
                        _ => {}
                    }),
                );
                card.append(&open);
            }
            self.listing.append(&card);
        }
    }

    /// Starts reading the manifest of whatever image the field names.
    ///
    /// Nothing is recorded and nothing is asked yet: this only reads.
    pub fn inspect(self: &Rc<Self>) {
        let reference = self.reference.text().trim().to_owned();
        self.forget();
        if reference.is_empty() {
            self.say(
                "Enter an OCI image, for example acme/my-extension:1.2.3 or registry.example.com/team/extension:1.2.3",
            );
            return;
        }
        if reference.len() > 512 {
            self.say("Image references must be 512 characters or fewer");
            return;
        }
        let reference = match reference.parse::<hl_images::Reference>() {
            Ok(reference) => reference.to_string(),
            Err(_) => {
                self.say(
                    "That is not a valid OCI image reference. Try acme/my-extension:1.2.3, registry.example.com/team/extension:1.2.3, or a digest reference.",
                );
                return;
            }
        };
        self.reference.set_text(&reference);
        self.consent_focus.set(self.inspect.has_focus());
        self.say(&format!("reading {reference}"));
        self.reference.set_sensitive(false);
        self.inspect.set_sensitive(false);
        self.inspect.set_label("Reading…");
        self.cancel.set_label("Cancel download");
        self.cancel.set_sensitive(true);
        self.cancel.set_visible(true);
        self.acquisition_started(&reference);
        *self.pending.borrow_mut() = Some((self.inspection)(&reference));
    }

    pub fn cancel(&self) {
        let pending = self.pending.borrow();
        let Some(pending) = pending.as_ref() else { return };
        pending.cancellation.cancel();
        self.cancel.set_label("Cancelling…");
        self.cancel.set_sensitive(false);
        self.semantics.update(
            "extensions/acquisition/cancel",
            super::super::semantic::Value::Public("Cancellation requested"),
            true,
        );
        self.say("cancelling image acquisition");
    }

    /// Looks once for an inspection that has come back.
    ///
    /// Returns whether one was applied, which is what a test waits on instead
    /// of a clock.
    pub fn poll(self: &Rc<Self>) -> bool {
        let Some(event) = self.received() else {
            return false;
        };
        if self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.cancellation.is_cancelled())
            && !matches!(event, Acquisition::Cancelled)
        {
            self.cancelled();
            return true;
        }
        match event {
            Acquisition::Inspecting => self.stage("checking local images"),
            Acquisition::Pulling {
                status,
                id,
                current,
                total,
            } => self.pulling(&status, id.as_deref(), current, total),
            Acquisition::ReadingManifest => self.stage("reading extension manifest"),
            Acquisition::Ready(candidate) => {
                *self.pending.borrow_mut() = None;
                self.acquisition_finished();
                self.progress.set_visible(false);
                self.cancel.set_visible(false);
                self.reference.set_sensitive(true);
                self.inspect.set_sensitive(true);
                self.inspect.set_label("Read another image");
                let installed = self
                    .shelf
                    .roster()
                    .borrow()
                    .entries()
                    .iter()
                    .any(|entry| entry.name == candidate.manifest.name);
                if installed {
                    let prepared = self
                        .shelf
                        .roster()
                        .borrow()
                        .prepare_update(&candidate.manifest, &candidate.digest);
                    match prepared {
                        Ok(update) => self.propose_update(candidate, update),
                        Err(refusal) => self.say(&refusal.to_string()),
                    }
                } else {
                    self.propose_install(candidate);
                }
                if self.consent_focus.replace(false) {
                    self.proposal.child_focus(gtk::DirectionType::TabForward);
                }
            }
            Acquisition::Failed(reason) => {
                *self.pending.borrow_mut() = None;
                self.acquisition_finished();
                self.progress.set_visible(false);
                self.cancel.set_visible(false);
                self.reference.set_sensitive(true);
                self.inspect.set_sensitive(true);
                self.inspect.set_label("Retry");
                self.say(&reason);
                self.offer_retry();
            }
            Acquisition::Cancelled => {
                self.cancelled();
            }
        }
        true
    }

    fn cancelled(&self) {
        *self.pending.borrow_mut() = None;
        self.progress.set_visible(false);
        self.cancel.set_visible(false);
        self.reference.set_sensitive(true);
        self.inspect.set_sensitive(true);
        self.inspect.set_label("Retry");
        self.say("image acquisition cancelled; nothing was installed");
        self.acquisition_finished();
        self.offer_retry();
    }

    /// Records the grant for the candidate on screen.
    ///
    /// The consent recorded is exactly what was shown, and the roster narrows
    /// it again to what the manifest declares.
    pub fn consent(self: &Rc<Self>) {
        let Some(proposal) = self.candidate.borrow().clone() else {
            self.say("there is nothing to install");
            return;
        };
        match proposal {
            Proposal::Install { candidate, selected } => {
                let consent = Grant::new(selected.borrow().iter().copied());
                let recorded = self.shelf.roster().borrow_mut().register(
                    &candidate.manifest,
                    &candidate.digest,
                    &consent,
                    moment(),
                );
                self.settle(&candidate, recorded);
            }
            Proposal::Update {
                candidate,
                update,
                selected,
            } => {
                let consent = Grant::new(selected.borrow().iter().copied());
                let recorded = self
                    .shelf
                    .roster()
                    .borrow_mut()
                    .commit_update(update, &consent, moment());
                if let Err(refusal) = recorded {
                    // The prepared update is generation-bound. Once it loses
                    // that race, repeating consent can never make it current;
                    // withdraw the stale authority and require a fresh image
                    // inspection against the installed winner.
                    self.forget();
                    self.inspect.set_label("Read manifest again");
                    self.offer_retry();
                    self.say(&format!(
                        "update failed; the installed extension is unchanged: {refusal}. Read the manifest again before consenting"
                    ));
                    return;
                }
                self.shelf.refresh(&candidate.manifest.name);
                self.forget();
                self.refresh();
                self.say(&format!(
                    "{} was updated from {} at {}",
                    candidate.manifest.name, candidate.reference, candidate.digest
                ));
            }
        }
    }

    /// Walks away from the candidate, recording nothing.
    pub fn decline(self: &Rc<Self>) {
        self.decline_with_focus(false);
    }

    /// Walks away from a keyboard-focused candidate without dropping focus
    /// along with the controls that represented it.
    fn decline_with_focus(self: &Rc<Self>, restore_focus: bool) {
        self.forget();
        self.say("nothing changed");
        if restore_focus {
            self.inspect.grab_focus();
        }
    }

    /// Puts the candidate on the shelf, or says why it could not go there.
    fn settle(self: &Rc<Self>, candidate: &Candidate, recorded: Result<(), hl::extension::Refusal>) {
        if let Err(refusal) = recorded {
            self.say(&refusal.to_string());
            return;
        }
        let entry = self
            .shelf
            .roster()
            .borrow()
            .entries()
            .into_iter()
            .find(|entry| entry.name == candidate.manifest.name);
        if let Some(entry) = entry {
            // Installation deliberately records a disabled extension. Put the
            // lifecycle controls in front of the person who just installed it
            // so appearing in the sidebar cannot be mistaken for activation.
            self.shelf.refresh(&entry.name);
        }
        self.forget();
        self.refresh();
        self.say(&format!(
            "{} is installed and disabled from {} at {}. Choose Enable to start it",
            candidate.manifest.name, candidate.reference, candidate.digest
        ));
    }

    /// Shows what an image asks for, and asks.
    fn propose_install(self: &Rc<Self>, candidate: Candidate) {
        let manifest = &candidate.manifest;
        self.proposal.append(&text(
            &format!("{} {} — {}", manifest.name, manifest.version, manifest.display_name),
            "dhead",
        ));
        self.proposal
            .append(&text(&format!("Image: {}", candidate.reference), "fhint"));
        self.proposal
            .append(&text(&format!("Digest: {}", candidate.digest), "fhint"));
        let summary = Summary::of(&manifest.capabilities);
        if summary.execution {
            self.proposal.append(&text(Summary::EXECUTION_NOTICE, "fhint"));
        }
        let selected = self.selection(manifest, manifest.capabilities.iter());
        let capabilities = gtk::Box::new(gtk::Orientation::Vertical, 4);
        capabilities.add_css_class(PROPOSAL_CAPABILITIES);
        capabilities.set_accessible_role(gtk::AccessibleRole::List);
        for capability in manifest.capabilities.iter() {
            let item = gtk::Box::new(gtk::Orientation::Vertical, 0);
            item.set_accessible_role(gtk::AccessibleRole::ListItem);
            item.append(&self.capability_choice(manifest, capability, &selected));
            capabilities.append(&item);
        }
        self.proposal.append(&capabilities);
        self.selected_semantics("Selected capabilities", &selected);
        self.proposal.append(&self.answer("Install"));
        self.proposal.set_visible(true);
        let summary = format!("{} {} at {}", manifest.name, manifest.version, candidate.digest);
        self.semantics.register(
            "extensions/proposal/summary",
            "dialog",
            Some("Install extension"),
            Some(super::super::semantic::Value::Public(&summary)),
            &[],
            Rc::new(|_, _| {}),
        );
        self.semantics.register(
            "extensions/proposal/capabilities",
            "list",
            Some("Requested capabilities"),
            Some(super::super::semantic::Value::Public(&capability_list(
                manifest.capabilities.iter(),
            ))),
            &[],
            Rc::new(|_, _| {}),
        );
        *self.candidate.borrow_mut() = Some(Proposal::Install { candidate, selected });
        self.say("this image asks for the capabilities above; select what to grant; required interface access cannot be removed");
    }

    fn propose_update(self: &Rc<Self>, candidate: Candidate, update: Update) {
        self.proposal.append(&text(&format!("Update {}", update.name), "dhead"));
        self.proposal.append(&text(
            &format!(
                "installed  {}  ·  {}",
                if update.current_version.is_empty() {
                    "unknown version"
                } else {
                    &update.current_version
                },
                update.current_digest
            ),
            "fhint",
        ));
        self.proposal.append(&text(
            &format!(
                "candidate  {}  ·  {}",
                update.candidate_version, update.candidate_digest
            ),
            "fhint",
        ));
        if update.additional.is_empty() && update.removed.is_empty() {
            self.proposal.append(&text("capabilities unchanged", UPDATE_DELTA));
        }
        let selected = self.selection(&candidate.manifest, update.additional.iter().copied());
        let added = gtk::Box::new(gtk::Orientation::Vertical, 4);
        added.add_css_class(UPDATE_CAPABILITIES);
        added.set_accessible_role(gtk::AccessibleRole::List);
        for capability in &update.additional {
            let item = gtk::Box::new(gtk::Orientation::Vertical, 2);
            item.set_accessible_role(gtk::AccessibleRole::ListItem);
            item.append(&text(&format!("+ {}", capability.as_str()), UPDATE_DELTA));
            item.append(&self.capability_choice(&candidate.manifest, *capability, &selected));
            added.append(&item);
        }
        self.proposal.append(&added);
        self.selected_semantics("Selected additional capabilities", &selected);
        for capability in &update.removed {
            self.proposal
                .append(&text(&format!("− {}", capability.as_str()), UPDATE_DELTA));
        }
        let summary = Summary::of(&Grant::new(update.additional.iter().copied()));
        if summary.execution {
            self.proposal.append(&text(Summary::EXECUTION_NOTICE, "fhint"));
        }
        self.proposal.append(&self.answer("Accept update"));
        self.proposal.set_visible(true);
        let summary = format!(
            "{} {} {} to {} {}; added {}; removed {}",
            update.name,
            update.current_version,
            update.current_digest,
            update.candidate_version,
            update.candidate_digest,
            update.additional.len(),
            update.removed.len()
        );
        self.semantics.register(
            "extensions/proposal/summary",
            "dialog",
            Some("Update extension"),
            Some(super::super::semantic::Value::Public(&summary)),
            &[],
            Rc::new(|_, _| {}),
        );
        self.semantics.register(
            "extensions/proposal/added-capabilities",
            "list",
            Some("Added capabilities"),
            Some(super::super::semantic::Value::Public(&capability_list(
                update.additional.iter().copied(),
            ))),
            &[],
            Rc::new(|_, _| {}),
        );
        self.semantics.register(
            "extensions/proposal/removed-capabilities",
            "list",
            Some("Removed capabilities"),
            Some(super::super::semantic::Value::Public(&capability_list(
                update.removed.iter().copied(),
            ))),
            &[],
            Rc::new(|_, _| {}),
        );
        *self.candidate.borrow_mut() = Some(Proposal::Update {
            candidate,
            update,
            selected,
        });
        self.say("review the image changes and select which additional capabilities to grant");
    }

    fn selection(&self, manifest: &Manifest, capabilities: impl Iterator<Item = Capability>) -> Selection {
        Rc::new(RefCell::new(
            capabilities
                .filter(|capability| required(manifest, *capability))
                .collect(),
        ))
    }

    fn capability_choice(&self, manifest: &Manifest, capability: Capability, selected: &Selection) -> gtk::CheckButton {
        use super::super::semantic::{ActionKind, Value};
        let required = required(manifest, capability);
        let choice = gtk::CheckButton::with_label(capability.as_str());
        choice.add_css_class(CAPABILITY_CHOICE);
        choice.set_active(required);
        choice.set_sensitive(!required);
        let path = format!("extensions/proposal/capability/{}", capability.as_str());
        let toggled = choice.clone();
        let selection = Rc::clone(selected);
        let semantics = self.semantics.clone();
        let semantic_path = path.clone();
        let actions = if required {
            vec![ActionKind::Focus]
        } else {
            vec![ActionKind::Toggle, ActionKind::Focus]
        };
        self.semantics.register(
            &path,
            "checkbox",
            Some(capability.as_str()),
            Some(Value::Public(if required {
                "selected · required"
            } else {
                "not selected · optional"
            })),
            &actions,
            Rc::new(move |action, _| match action {
                ActionKind::Toggle if toggled.is_sensitive() => toggled.set_active(!toggled.is_active()),
                ActionKind::Focus => {
                    toggled.grab_focus();
                }
                _ => {}
            }),
        );
        choice.connect_toggled(move |choice| {
            let mut selection = selection.borrow_mut();
            if choice.is_active() {
                selection.insert(capability);
            } else {
                selection.remove(&capability);
            }
            let selected = capability_list(selection.iter().copied());
            drop(selection);
            semantics.update(
                &semantic_path,
                Value::Public(if choice.is_active() {
                    "selected · optional"
                } else {
                    "not selected · optional"
                }),
                false,
            );
            semantics.update(
                "extensions/proposal/selected-capabilities",
                Value::Public(&selected),
                false,
            );
        });
        choice
    }

    fn selected_semantics(&self, label: &str, selected: &Selection) {
        self.semantics.register(
            "extensions/proposal/selected-capabilities",
            "list",
            Some(label),
            Some(super::super::semantic::Value::Public(&capability_list(
                selected.borrow().iter().copied(),
            ))),
            &[],
            Rc::new(|_, _| {}),
        );
    }

    /// The two buttons a candidate is answered with.
    fn answer(self: &Rc<Self>, accept: &str) -> gtk::Box {
        use super::super::semantic::ActionKind;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let install = gtk::Button::with_label(accept);
        install.add_css_class(CONSENT);
        let page = Rc::downgrade(self);
        install.connect_clicked(move |_| {
            if let Some(page) = page.upgrade() {
                page.consent();
            }
        });
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class(DECLINE);
        let page = Rc::downgrade(self);
        let native_cancel = cancel.clone();
        cancel.connect_clicked(move |_| {
            if let Some(page) = page.upgrade() {
                page.decline_with_focus(native_cancel.has_focus());
            }
        });
        let page = Rc::downgrade(self);
        let semantic_install = install.clone();
        self.semantics.register(
            "extensions/proposal/consent",
            "button",
            Some(accept),
            None,
            &[ActionKind::Invoke, ActionKind::Focus],
            Rc::new(move |action, _| match action {
                ActionKind::Invoke => {
                    if let Some(page) = page.upgrade() {
                        page.consent();
                    }
                }
                ActionKind::Focus => {
                    semantic_install.grab_focus();
                }
                _ => {}
            }),
        );
        let page = Rc::downgrade(self);
        let semantic_cancel = cancel.clone();
        self.semantics.register(
            "extensions/proposal/cancel",
            "button",
            Some("Cancel"),
            None,
            &[ActionKind::Invoke, ActionKind::Focus],
            Rc::new(move |action, _| match action {
                ActionKind::Invoke => {
                    if let Some(page) = page.upgrade() {
                        page.decline_with_focus(semantic_cancel.has_focus());
                    }
                }
                ActionKind::Focus => {
                    semantic_cancel.grab_focus();
                }
                _ => {}
            }),
        );
        row.append(&install);
        row.append(&cancel);
        row
    }

    /// Drops the candidate and everything drawn about it.
    fn forget(&self) {
        self.semantics.remove_prefix("extensions/proposal/");
        *self.candidate.borrow_mut() = None;
        while let Some(child) = self.proposal.first_child() {
            self.proposal.remove(&child);
        }
        self.proposal.set_visible(false);
    }

    /// Reads the pending inspection without holding its borrow across the
    /// work that follows.
    fn received(&self) -> Option<Acquisition> {
        let pending = self.pending.borrow();
        let waiting = pending.as_ref()?;
        match waiting.events.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Acquisition::Failed("the image was never read".to_owned())),
        }
    }

    fn stage(&self, stage: &str) {
        self.progress.set_fraction(0.0);
        self.progress.set_text(Some(stage));
        self.progress.set_visible(true);
        self.say(stage);
        self.semantics.update(
            "extensions/acquisition/progress",
            super::super::semantic::Value::Public(stage),
            false,
        );
    }

    fn pulling(&self, status: &str, id: Option<&str>, current: Option<u64>, total: Option<u64>) {
        let said = id.map_or_else(|| status.to_owned(), |id| format!("{status} · {id}"));
        match (current, total) {
            (Some(current), Some(total)) if total != 0 => {
                self.progress
                    .set_fraction((current as f64 / total as f64).clamp(0.0, 1.0));
            }
            _ => self.progress.pulse(),
        }
        self.progress.set_text(Some(&said));
        self.progress.set_visible(true);
        self.say(&said);
        let semantic = match (current, total) {
            (Some(current), Some(total)) if total != 0 => {
                format!(
                    "{said}; {}%; {current} of {total} bytes",
                    current.saturating_mul(100) / total
                )
            }
            _ => format!("{said}; progress unavailable"),
        };
        self.semantics.update(
            "extensions/acquisition/progress",
            super::super::semantic::Value::Public(&semantic),
            false,
        );
    }

    fn acquisition_started(self: &Rc<Self>, reference: &str) {
        use super::super::semantic::{ActionKind, Value};
        self.semantics.remove_prefix("extensions/acquisition/");
        self.semantics
            .update("extensions/inspect", Value::Public("Acquisition in progress"), true);
        self.semantics.register(
            "extensions/acquisition/progress",
            "progressbar",
            Some("Image acquisition progress"),
            Some(Value::Public(&format!("Starting {reference}"))),
            &[],
            Rc::new(|_, _| {}),
        );
        let page = Rc::downgrade(self);
        let cancel = self.cancel.clone();
        self.semantics.register(
            "extensions/acquisition/cancel",
            "button",
            Some("Cancel download"),
            None,
            &[ActionKind::Invoke, ActionKind::Focus],
            Rc::new(move |action, _| match action {
                ActionKind::Invoke => {
                    if let Some(page) = page.upgrade() {
                        page.cancel();
                    }
                }
                ActionKind::Focus => {
                    cancel.grab_focus();
                }
                _ => {}
            }),
        );
    }

    fn acquisition_finished(&self) {
        self.semantics.remove_prefix("extensions/acquisition/");
        self.semantics.set_disabled("extensions/inspect", false);
    }

    fn offer_retry(&self) {
        // The existing inspect action remains the retry authority. Its value
        // tells semantic clients why it is currently offered without creating
        // a second action that can drift from the visible button.
        self.semantics.update(
            "extensions/inspect",
            super::super::semantic::Value::Public("Retry acquisition"),
            false,
        );
    }

    /// What the page last did, in one line.
    fn say(&self, said: &str) {
        self.notice.set_text(said);
        self.notice.set_visible(true);
        self.semantics
            .update("extensions/notice", super::super::semantic::Value::Public(said), false);
    }

    /// The line a test reads to see what the page said.
    #[must_use]
    pub fn notice(&self) -> String {
        self.notice.text().to_string()
    }

    /// Lays the page out and puts its polling on the main loop.
    fn assemble(self: &Rc<Self>) {
        use super::super::semantic::{ActionKind, Value};
        let title = text("Extensions", "dashtitle");
        title.set_accessible_role(gtk::AccessibleRole::Heading);
        self.widget.append(&title);
        self.widget.append(&text("Installed", "dhead"));
        self.widget.append(&self.listing);
        self.widget.append(&text("Register an image", "dhead"));
        self.reference.add_css_class(REFERENCE);
        // GTK entries otherwise contribute the full placeholder as their
        // minimum width, clipping the whole page beside compact navigation.
        self.reference.set_width_chars(1);
        self.reference.set_hexpand(true);
        self.reference
            .set_placeholder_text(Some("OCI image, e.g. registry.example.com/team/extension:1.2.3"));
        self.semantics.register(
            "extensions/reference",
            "textbox",
            Some("Extension image"),
            Some(Value::Public("")),
            &[ActionKind::Change, ActionKind::Focus],
            {
                let input = self.reference.clone();
                Rc::new(move |action, value| match action {
                    ActionKind::Change => input.set_text(value.unwrap_or_default()),
                    ActionKind::Focus => {
                        input.grab_focus();
                    }
                    _ => {}
                })
            },
        );
        {
            let registry = self.semantics.clone();
            self.reference.connect_changed(move |input| {
                let value = input.text();
                registry.update(
                    "extensions/reference",
                    Value::Public(value.as_str()),
                    !input.is_sensitive(),
                );
            });
        }
        self.widget.append(&self.reference);
        self.widget.append(&text(
            "Docker Hub or private registry: acme/my-extension:1.2.3 · registry.example.com/team/extension:1.2.3 · digest references supported",
            "dhint",
        ));
        self.inspect.add_css_class(INSPECT);
        let page = Rc::downgrade(self);
        self.inspect.connect_clicked(move |_| {
            if let Some(page) = page.upgrade() {
                page.inspect();
            }
        });
        let inspect = Rc::downgrade(self);
        let inspect_button = self.inspect.clone();
        self.semantics.register(
            "extensions/inspect",
            "button",
            Some("Read manifest"),
            None,
            &[ActionKind::Invoke, ActionKind::Focus],
            Rc::new(move |action, _| match action {
                ActionKind::Invoke => {
                    if let Some(inspect) = inspect.upgrade() {
                        inspect.inspect();
                    }
                }
                ActionKind::Focus => {
                    inspect_button.grab_focus();
                }
                _ => {}
            }),
        );
        self.semantics.register(
            "extensions/notice",
            "status",
            Some("Extension status"),
            Some(Value::Public("")),
            &[],
            Rc::new(|_, _| {}),
        );
        self.widget.append(&self.inspect);
        self.widget.append(&self.progress);
        let page = Rc::downgrade(self);
        self.cancel.connect_clicked(move |_| {
            if let Some(page) = page.upgrade() {
                page.cancel();
            }
        });
        self.widget.append(&self.cancel);
        self.widget.append(&self.proposal);
        self.widget.append(&self.notice);
        self.tick();
    }

    /// Looks for a finished inspection until the page is gone.
    fn tick(self: &Rc<Self>) {
        let page = Rc::downgrade(self);
        gtk::glib::timeout_add_local(TICK, move || {
            let Some(page) = page.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            page.poll();
            gtk::glib::ControlFlow::Continue
        });
    }
}

impl Drop for Catalogue {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.get_mut().as_ref() {
            pending.cancellation.cancel();
        }
        self.semantics.remove_prefix("extensions/");
    }
}

fn required(manifest: &Manifest, capability: Capability) -> bool {
    capability == Capability::Interface && (manifest.interface.is_some() || !manifest.pane_providers.is_empty())
}

/// A finite, readable capability list for the consent projection.
fn capability_list(capabilities: impl IntoIterator<Item = hl_extension::Capability>) -> String {
    let capabilities = capabilities
        .into_iter()
        .map(hl_extension::Capability::as_str)
        .collect::<Vec<_>>();
    if capabilities.is_empty() {
        return "none".to_owned();
    }
    capabilities.join(", ")
}

/// One line of text on the page.
fn text(said: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(said));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label
}
