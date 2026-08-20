//! The workspace page listing every extension, and the only way to add one.
//!
//! Adding an extension is two steps on purpose. The image is read first and
//! nothing is written; what it asks for is shown; and only an answer a person
//! gives records a grant. There is no path on this page from an image's request
//! to a recorded grant that skips the middle step.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};

use gtk::prelude::*;
use hl::extension::Candidate;
use hl_extension::{Stage, Summary};

use super::{moment, Inspection, Shelf};

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
/// Style class on the block describing a candidate image.
pub const PROPOSAL: &str = "hl-extension-proposal";

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
    proposal: gtk::Box,
    notice: gtk::Label,
    /// The inspection in flight, if any. One at a time, because the field it
    /// was started from is the same field a second one would read.
    pending: RefCell<Option<Receiver<Result<Candidate, String>>>>,
    /// What the last inspection found, waiting for an answer.
    candidate: RefCell<Option<Candidate>>,
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
        let page = Rc::new(Self {
            widget,
            shelf: Rc::clone(shelf),
            inspection,
            listing: gtk::Box::new(gtk::Orientation::Vertical, 4),
            reference: gtk::Entry::new(),
            proposal,
            notice,
            pending: RefCell::new(None),
            candidate: RefCell::new(None),
        });
        page.assemble();
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
                .append(&text(&format!("{}  ·  {}", entry.name, said(entry.stage)), "fhint"));
        }
    }

    /// Starts reading the manifest of whatever image the field names.
    ///
    /// Nothing is recorded and nothing is asked yet: this only reads.
    pub fn inspect(self: &Rc<Self>) {
        let reference = self.reference.text().trim().to_owned();
        if reference.is_empty() {
            self.say("type the image to register first");
            return;
        }
        self.forget();
        self.say(&format!("reading {reference}"));
        *self.pending.borrow_mut() = Some((self.inspection)(&reference));
    }

    /// Looks once for an inspection that has come back.
    ///
    /// Returns whether one was applied, which is what a test waits on instead
    /// of a clock.
    pub fn poll(self: &Rc<Self>) -> bool {
        let taken = self.received();
        let Some(answer) = taken else {
            return false;
        };
        *self.pending.borrow_mut() = None;
        match answer {
            Ok(candidate) => self.propose(&candidate),
            Err(reason) => self.say(&reason),
        }
        true
    }

    /// Records the grant for the candidate on screen.
    ///
    /// The consent recorded is exactly what was shown, and the roster narrows
    /// it again to what the manifest declares.
    pub fn consent(self: &Rc<Self>) {
        let Some(candidate) = self.candidate.borrow().clone() else {
            self.say("there is nothing to install");
            return;
        };
        let recorded = self.shelf.roster().borrow_mut().register(
            &candidate.manifest,
            &candidate.digest,
            &candidate.manifest.capabilities,
            moment(),
        );
        self.settle(&candidate, recorded);
    }

    /// Walks away from the candidate, recording nothing.
    pub fn decline(self: &Rc<Self>) {
        self.forget();
        self.say("nothing was installed");
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
        self.say(&format!("{} is installed and disabled", candidate.manifest.name));
    }

    /// Shows what an image asks for, and asks.
    fn propose(self: &Rc<Self>, candidate: &Candidate) {
        let manifest = &candidate.manifest;
        self.proposal.append(&text(
            &format!("{} {} — {}", manifest.name, manifest.version, manifest.display_name),
            "dhead",
        ));
        self.proposal.append(&text(&candidate.digest, "fhint"));
        let summary = Summary::of(&manifest.capabilities);
        if summary.execution {
            self.proposal.append(&text(Summary::EXECUTION_NOTICE, "fhint"));
        }
        for capability in manifest.capabilities.iter() {
            self.proposal.append(&text(capability.as_str(), "fhint"));
        }
        self.proposal.append(&self.answer());
        self.proposal.set_visible(true);
        *self.candidate.borrow_mut() = Some(candidate.clone());
        self.say("this image asks for the capabilities above");
    }

    /// The two buttons a candidate is answered with.
    fn answer(self: &Rc<Self>) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let install = gtk::Button::with_label("Install");
        install.add_css_class(CONSENT);
        let page = Rc::clone(self);
        install.connect_clicked(move |_| page.consent());
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class(DECLINE);
        let page = Rc::clone(self);
        cancel.connect_clicked(move |_| page.decline());
        row.append(&install);
        row.append(&cancel);
        row
    }

    /// Drops the candidate and everything drawn about it.
    fn forget(&self) {
        *self.candidate.borrow_mut() = None;
        while let Some(child) = self.proposal.first_child() {
            self.proposal.remove(&child);
        }
        self.proposal.set_visible(false);
    }

    /// Reads the pending inspection without holding its borrow across the
    /// work that follows.
    fn received(&self) -> Option<Result<Candidate, String>> {
        let pending = self.pending.borrow();
        let waiting = pending.as_ref()?;
        match waiting.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("the image was never read".to_owned())),
        }
    }

    /// What the page last did, in one line.
    fn say(&self, said: &str) {
        self.notice.set_text(said);
        self.notice.set_visible(true);
    }

    /// The line a test reads to see what the page said.
    #[must_use]
    pub fn notice(&self) -> String {
        self.notice.text().to_string()
    }

    /// Lays the page out and puts its polling on the main loop.
    fn assemble(self: &Rc<Self>) {
        self.widget.append(&text("Installed", "dhead"));
        self.widget.append(&self.listing);
        self.widget.append(&text("Register an image", "dhead"));
        self.reference.add_css_class(REFERENCE);
        self.reference.set_placeholder_text(Some("image reference"));
        self.widget.append(&self.reference);
        let read = gtk::Button::with_label("Read manifest");
        read.add_css_class(INSPECT);
        let page = Rc::clone(self);
        read.connect_clicked(move |_| page.inspect());
        self.widget.append(&read);
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

/// Where an extension stands, in the words the listing uses.
fn said(stage: Stage) -> String {
    match stage {
        Stage::Vacancy => "not installed".to_owned(),
        Stage::Standby => "disabled".to_owned(),
        Stage::Duty => "enabled".to_owned(),
        Stage::Fault { restarts } => format!("faulted after {restarts} restarts"),
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
