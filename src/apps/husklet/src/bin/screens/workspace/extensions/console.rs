//! Where an extension's terminal requests are actually carried out.
//!
//! The port an extension holds is [`hl::extension::Relay`], which only carries
//! the request to whichever thread is drawing. This is that thread's half: it
//! drains the errands on the window's own tick and answers each one from the
//! widgets, which is the only place the answer exists.

use std::rc::Rc;

use hl::extension::{Answer, Errand, Errands, Request};
use hl_extension::port::{Division, HostError, PaneSummary, PaneText, TabSummary};
use vte4::prelude::*;

use super::super::terminal::{Adjustment, Occupancy, PaneView, Panes, Reading, Slots, Surface, Tabs, TermWin, Window};

/// How often the window looks for errands.
///
/// The same rhythm the other live pages run at, so an extension asking for a
/// pane cannot make the window busier than drawing already does.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// The window's half of the terminal port.
///
/// The window is held weakly, because the pages it draws are inside it: a
/// strong reference here would be a cycle nothing breaks, and the window would
/// outlive its own closing.
pub(crate) struct Console {
    window: std::rc::Weak<TermWin>,
    errands: Errands,
}

impl Console {
    /// Binds the errands of one workspace's extensions to one terminal window.
    #[must_use]
    pub(crate) fn new(window: &Rc<TermWin>, errands: Errands) -> Self {
        Self {
            window: Rc::downgrade(window),
            errands,
        }
    }

    /// Puts the draining on the main loop. It ends with the window.
    pub(crate) fn install(self) {
        gtk::glib::timeout_add_local(TICK, move || {
            if self.drain().is_none() {
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    /// Answers everything waiting, and returns how many were answered.
    ///
    /// `None` means the window has closed, which is the end of the draining
    /// rather than a failure. Bounded by what is queued rather than by a cap,
    /// because each errand is one extension waiting on a blocked call and the
    /// relay's own queue is already small.
    pub(crate) fn drain(&self) -> Option<usize> {
        let window = self.window.upgrade()?;
        let mut answered = 0;
        while let Ok(errand) = self.errands.try_recv() {
            Self::serve(&window, errand);
            answered += 1;
        }
        Some(answered)
    }

    /// Carries out one errand and answers it.
    fn serve(window: &Rc<TermWin>, errand: Errand) {
        let answer = match errand.request() {
            Request::Tabs => Ok(Answer::Tabs(Self::tabs(window))),
            Request::OpenTab(title) => Ok(Answer::Slot(Self::open(window, title))),
            Request::Split { slot, division } => Self::split(window, slot, *division).map(Answer::Slot),
            Request::Spawn { slot, command } => Self::spawn(window, slot, command).map(|()| Answer::Done),
            Request::Read { slot, lines } => Self::read(window, slot, *lines).map(Answer::Text),
            Request::Close { slot } => Self::close(window, slot).map(|()| Answer::Done),
            Request::Focus { slot } => Self::focus(window, slot).map(|()| Answer::Done),
            Request::Ratio { slot, ratio } => Self::ratio(window, slot, *ratio).map(|()| Answer::Done),
            Request::Surface { origin, slot, division } => {
                Self::surface(window, origin.as_deref(), slot, *division).map(Answer::Slot)
            }
        };
        errand.answer(answer);
    }

    /// Every tab and the panes in it.
    fn tabs(window: &Rc<TermWin>) -> Vec<TabSummary> {
        Window::tabs(window)
            .into_iter()
            .map(|(name, widget, _)| TabSummary {
                id: name.clone(),
                title: name,
                panes: Panes::under(window, &widget).into_iter().map(pane).collect(),
            })
            .collect()
    }

    /// A bounded tail of what one pane is showing.
    ///
    /// The bound arrives already applied by the protocol layer and is carried
    /// into the extraction itself, so the window never builds an answer larger
    /// than the one it is allowed to send.
    fn read(window: &Rc<TermWin>, slot: &str, lines: usize) -> Result<PaneText, HostError> {
        match Panes::read(window, slot, lines) {
            Reading::Text(text) => Ok(text),
            Reading::Drawn => Err(HostError::Conflict(format!(
                "{slot} draws an interface, so it is showing no terminal text"
            ))),
            Reading::Absent => Err(absent(slot)),
        }
    }

    /// Closes one pane. The last pane of a tab takes the tab with it, which is
    /// what closing that pane by hand already does.
    fn close(window: &Rc<TermWin>, slot: &str) -> Result<(), HostError> {
        if Panes::close(window, slot) {
            return Ok(());
        }
        Err(absent(slot))
    }

    /// Moves keyboard focus to one pane.
    fn focus(window: &Rc<TermWin>, slot: &str) -> Result<(), HostError> {
        if Panes::focus(window, slot) {
            return Ok(());
        }
        // A pane that exists but refused focus is not a pane an extension can
        // be told anything useful about, so it is reported the same way.
        Err(absent(slot))
    }

    /// Sets how much of its split one pane takes.
    fn ratio(window: &Rc<TermWin>, slot: &str, ratio: f64) -> Result<(), HostError> {
        match Panes::ratio(window, slot, ratio) {
            Adjustment::Set => Ok(()),
            Adjustment::Whole => Err(HostError::Conflict(format!("{slot} is not inside a split"))),
            Adjustment::Absent => Err(absent(slot)),
        }
    }

    /// Divides one pane and gives the new half to an extension to draw in.
    ///
    /// The extension is named by the port the call arrived on. A call that
    /// names none cannot be answered: the window would have to guess whose
    /// interface belongs in the pane it just made.
    pub(super) fn surface(
        window: &Rc<TermWin>,
        origin: Option<&str>,
        slot: &str,
        division: Division,
    ) -> Result<String, HostError> {
        let origin = origin.ok_or_else(|| {
            HostError::Conflict("a pane that draws an interface must be asked for by an extension".to_owned())
        })?;
        // Resolve both ends before borrowing the extension's one interface. A
        // missing target must leave an existing surface exactly where it was.
        if Panes::at(window, slot).is_none() {
            return Err(absent(slot));
        }
        let previous = Surface::of(window, origin);
        let held = Window::slot(window);
        let content = Surface::build(window, origin, held.clone());
        if Panes::divide(window, slot, orientation(division), &content) {
            // One reconciliation stream owns one widget tree. Once the new
            // half is real, collapse the old holder; `build` already moved its
            // interface into `content`, so closing it cannot send it home.
            if let Some(previous) = previous {
                if let Some((old, _)) = Slots::new(window).surface(&previous) {
                    let _ = Panes::close(window, &old);
                }
            }
            return Ok(held);
        }
        // Nothing took the pane. Give up its registration, then return the
        // borrowed widget to the old surface rather than changing visible
        // location on a failed request.
        Surface::discard(window, &content);
        if let Some(previous) = previous {
            Surface::restore(window, origin, &previous);
        }
        Err(absent(slot))
    }

    /// Opens a shell tab and names it back.
    ///
    /// The title an extension asked for is not the tab's label: tabs in this
    /// window are shells and are labelled as such, and an extension naming
    /// someone else's tab would be drawing on a surface it does not own.
    fn open(window: &Rc<TermWin>, _title: &str) -> String {
        Tabs::new(window).terminal()
    }

    /// Divides one pane and names the pane that appeared.
    ///
    /// The new slot is found by comparing the window's slots before and after,
    /// because the split itself is the window's own action and does not report
    /// what it allocated.
    fn split(window: &Rc<TermWin>, slot: &str, division: Division) -> Result<String, HostError> {
        let terminal = Window::pane(window, slot).ok_or_else(|| absent(slot))?;
        let before = Self::slots(window);
        PaneView::new(window, &terminal).split(orientation(division));
        Self::slots(window)
            .into_iter()
            .find(|slot| !before.contains(slot))
            .ok_or_else(|| HostError::Failed(format!("{slot} could not be divided")))
    }

    /// Types a command into one pane, which is what running something in a
    /// shell someone else owns actually is.
    fn spawn(window: &Rc<TermWin>, slot: &str, command: &[String]) -> Result<(), HostError> {
        let terminal = Window::pane(window, slot).ok_or_else(|| absent(slot))?;
        if command.is_empty() {
            return Err(HostError::Conflict("no command was given".to_owned()));
        }
        terminal.feed_child(format!("{}\n", command.join(" ")).as_bytes());
        Ok(())
    }

    /// Every pane slot the window currently holds.
    fn slots(window: &Rc<TermWin>) -> Vec<String> {
        Window::tabs(window)
            .into_iter()
            .flat_map(|(_, _, slots)| slots)
            .collect()
    }
}

/// One pane as an extension sees it.
///
/// The working directory and the running command are left unsaid rather than
/// guessed: the window knows a pane's shell, not what that shell is doing.
fn pane(occupancy: Occupancy) -> PaneSummary {
    PaneSummary {
        slot: occupancy.slot,
        working_directory: None,
        command: None,
        occupant: occupancy.occupant,
    }
}

/// Which way a division divides, in the toolkit's own words.
const fn orientation(division: Division) -> gtk::Orientation {
    match division {
        Division::Beside => gtk::Orientation::Horizontal,
        Division::Below => gtk::Orientation::Vertical,
    }
}

/// Said the same way whenever a slot names no live pane.
fn absent(slot: &str) -> HostError {
    HostError::Absent(format!("no pane is open under {slot}"))
}
