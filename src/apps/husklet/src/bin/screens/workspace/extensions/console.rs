//! Where an extension's terminal requests are actually carried out.
//!
//! The port an extension holds is [`hl::extension::Relay`], which only carries
//! the request to whichever thread is drawing. This is that thread's half: it
//! drains the errands on the window's own tick and answers each one from the
//! widgets, which is the only place the answer exists.

use std::rc::Rc;

use hl::extension::{Answer, Errand, Errands, Request};
use hl_extension::port::{Division, HostError, PaneSummary, TabSummary};
use vte4::prelude::*;

use super::super::terminal::{PaneView, Tabs, TermWin, Window};

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
        };
        errand.answer(answer);
    }

    /// Every tab and the panes in it.
    fn tabs(window: &Rc<TermWin>) -> Vec<TabSummary> {
        Window::tabs(window)
            .into_iter()
            .map(|(name, _, slots)| TabSummary {
                id: name.clone(),
                title: name,
                panes: slots.into_iter().map(pane).collect(),
            })
            .collect()
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
        let orientation = match division {
            Division::Beside => gtk::Orientation::Horizontal,
            Division::Below => gtk::Orientation::Vertical,
        };
        PaneView::new(window, &terminal).split(orientation);
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
fn pane(slot: String) -> PaneSummary {
    PaneSummary {
        slot,
        working_directory: None,
        command: None,
    }
}

/// Said the same way whenever a slot names no live pane.
fn absent(slot: &str) -> HostError {
    HostError::Absent(format!("no pane is open under {slot}"))
}
