//! The workspace page listing every extension, and the only way to add one.
//!
//! Adding an extension is two steps on purpose. The image is read first and
//! nothing is written; what it asks for is shown; and only an answer a person
//! gives records a grant. There is no path on this page from an image's request
//! to a recorded grant that skips the middle step.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;

use gtk::prelude::*;
use hl::extension::{Acquisition, Candidate};
use hl_extension::{Grant, Summary, Update};

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
pub const UPDATE_DELTA: &str = "hl-extension-update-delta";

#[derive(Clone)]
enum Proposal {
    Install(Candidate),
    Update { candidate: Candidate, update: Update },
}

/// How often the page looks for an inspection that has come back.
///
/// Matches the other live workspace pages, so reading one image's manifest does
/// not set the application's rhythm.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// The "Extensions" page: what is installed, and how to install another.
pub struct Catalogue {
    widget: gtk::Box,
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
    semantics: super::super::semantic::Registry,
}

impl Catalogue {
    /// Builds the page and puts its polling on the main loop.
    #[must_use]
    pub fn new(shelf: &Rc<Shelf>, inspection: Inspection) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 12);
        widget.add_css_class("dmain");
        let notice = text("", NOTICE);
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

    /// Redraws the listing from what the roster now says.
    pub fn refresh(&self) {
        self.semantics.remove_prefix("extensions/installed/");
        while let Some(child) = self.listing.first_child() {
            self.listing.remove(&child);
        }
        let entries = self.shelf.roster().borrow().entries();
        if entries.is_empty() {
            self.listing.append(&text("— none installed —", "dhint"));
            return;
        }
        for entry in entries {
            self.listing
                .append(&super::settings::Settings::page(&self.shelf, &entry, &self.semantics));
        }
    }

    /// Starts reading the manifest of whatever image the field names.
    ///
    /// Nothing is recorded and nothing is asked yet: this only reads.
    pub fn inspect(self: &Rc<Self>) {
        let reference = self.reference.text().trim().to_owned();
        self.forget();
        if reference.is_empty() {
            self.say("Enter a Docker Hub image, for example acme/my-extension:1.2.3");
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
                    "That is not a valid image reference. Try acme/my-extension:1.2.3 or acme/my-extension@sha256:…",
                );
                return;
            }
        };
        self.reference.set_text(&reference);
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
            Proposal::Install(candidate) => {
                let recorded = self.shelf.roster().borrow_mut().register(
                    &candidate.manifest,
                    &candidate.digest,
                    &candidate.manifest.capabilities,
                    moment(),
                );
                self.settle(&candidate, recorded);
            }
            Proposal::Update { candidate, update } => {
                let consent = Grant::new(update.additional.iter().copied());
                let recorded = self
                    .shelf
                    .roster()
                    .borrow_mut()
                    .commit_update(update, &consent, moment());
                if let Err(refusal) = recorded {
                    self.say(&format!(
                        "update failed; the installed extension is unchanged: {refusal}"
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
        self.forget();
        self.say("nothing changed");
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
            self.shelf.mount(&entry);
        }
        self.forget();
        self.refresh();
        self.say(&format!(
            "{} is installed and disabled from {} at {}",
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
        for capability in manifest.capabilities.iter() {
            self.proposal.append(&text(capability.as_str(), "fhint"));
        }
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
        *self.candidate.borrow_mut() = Some(Proposal::Install(candidate));
        self.say("this image asks for the capabilities above");
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
        for capability in &update.additional {
            self.proposal
                .append(&text(&format!("+ {}", capability.as_str()), UPDATE_DELTA));
        }
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
        *self.candidate.borrow_mut() = Some(Proposal::Update { candidate, update });
        self.say("review the installed and candidate image changes before accepting");
    }

    /// The two buttons a candidate is answered with.
    fn answer(self: &Rc<Self>, accept: &str) -> gtk::Box {
        use super::super::semantic::ActionKind;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let install = gtk::Button::with_label(accept);
        install.add_css_class(CONSENT);
        let page = Rc::clone(self);
        install.connect_clicked(move |_| page.consent());
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class(DECLINE);
        let page = Rc::clone(self);
        cancel.connect_clicked(move |_| page.decline());
        let page = Rc::clone(self);
        self.semantics.register(
            "extensions/proposal/consent",
            "button",
            Some(accept),
            None,
            &[ActionKind::Invoke],
            Rc::new(move |_, _| page.consent()),
        );
        let page = Rc::clone(self);
        self.semantics.register(
            "extensions/proposal/cancel",
            "button",
            Some("Cancel"),
            None,
            &[ActionKind::Invoke],
            Rc::new(move |_, _| page.decline()),
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
        let page = Rc::clone(self);
        self.semantics.register(
            "extensions/acquisition/cancel",
            "button",
            Some("Cancel download"),
            None,
            &[ActionKind::Invoke],
            Rc::new(move |_, _| page.cancel()),
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
        self.widget.append(&text("Installed", "dhead"));
        self.widget.append(&self.listing);
        self.widget.append(&text("Register an image", "dhead"));
        self.reference.add_css_class(REFERENCE);
        self.reference
            .set_placeholder_text(Some("Docker Hub image, e.g. acme/my-extension:1.2.3"));
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
            "Docker Hub examples: acme/my-extension:1.2.3 · acme/my-extension@sha256:…",
            "dhint",
        ));
        self.inspect.add_css_class(INSPECT);
        let page = Rc::clone(self);
        self.inspect.connect_clicked(move |_| page.inspect());
        let inspect = Rc::clone(self);
        self.semantics.register(
            "extensions/inspect",
            "button",
            Some("Read manifest"),
            None,
            &[ActionKind::Invoke],
            Rc::new(move |_, _| inspect.inspect()),
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
        let page = Rc::clone(self);
        self.cancel.connect_clicked(move |_| page.cancel());
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

/// One line of text on the page.
fn text(said: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(said));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}
