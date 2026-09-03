//! Panes as things addressed by slot, whatever they hold.
//!
//! The window's own actions reach a pane through the widget somebody clicked
//! in. An extension has no pointer and no widget: it has a slot, which is the
//! stable identity a layout is persisted with. Everything an extension can do
//! to a pane is therefore expressed here, once, over that identity, so the
//! window's half of the extension port never walks the widget tree itself.

use super::*;

use hl_extension::port::{Occupant, PaneText};

/// One pane as the window holds it.
pub(crate) struct Occupancy {
    /// Stable leaf chrome used by topology operations.
    pub(crate) widget: gtk::Widget,
    /// The terminal or extension surface currently drawn inside the chrome.
    pub(crate) content: gtk::Widget,
    /// The stable identity the layout persists it under.
    pub(crate) slot: String,
    /// What is in it.
    pub(crate) occupant: Occupant,
}

/// What became of a request to set how much of its split a pane takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Adjustment {
    /// No pane is open under that slot.
    Absent,
    /// The pane is the whole of its tab, so it has no share to set.
    Whole,
    /// The split was moved.
    Set,
}

/// What became of a request to read a pane's text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reading {
    /// No pane is open under that slot.
    Absent,
    /// The pane draws an interface, so it shows no text a terminal could hand over.
    Drawn,
    /// The text, bounded as asked.
    Text(PaneText),
}

/// Every pane of a window, addressed by slot.
pub(crate) struct Panes;

impl Panes {
    /// Every pane under one widget, in the order they are laid out.
    pub(crate) fn under(window: &Rc<TermWin>, widget: &gtk::Widget) -> Vec<Occupancy> {
        let mut found = Vec::new();
        Self::walk(window, widget, &mut found);
        found
    }

    /// Every pane the window holds, across all its tabs.
    pub(crate) fn all(window: &Rc<TermWin>) -> Vec<Occupancy> {
        let pages: Vec<gtk::Widget> = Window::tabs(window).into_iter().map(|(_, widget, _)| widget).collect();
        pages.iter().flat_map(|page| Self::under(window, page)).collect()
    }

    /// The pane open under one slot, if there is one.
    pub(crate) fn at(window: &Rc<TermWin>, slot: &str) -> Option<Occupancy> {
        Self::all(window).into_iter().find(|pane| pane.slot == slot)
    }

    /// A bounded tail of what one pane is showing.
    pub(crate) fn read(window: &Rc<TermWin>, slot: &str, lines: usize) -> Reading {
        let Some(pane) = Self::at(window, slot) else {
            return Reading::Absent;
        };
        let Ok(terminal) = pane.content.downcast::<vte4::Terminal>() else {
            return Reading::Drawn;
        };
        let (lines, truncated) = Terminal::new(&terminal).tail(lines);
        let (cursor_column, cursor_row) = terminal.cursor_position();
        Reading::Text(PaneText {
            slot: slot.to_owned(),
            generation: 0,
            revision: 0,
            lines,
            cursor_column: u32::try_from(cursor_column).unwrap_or_default(),
            cursor_row: u32::try_from(cursor_row).unwrap_or_default(),
            truncated,
        })
    }

    /// Closes one pane, which is what closing it by hand does.
    pub(crate) fn close(window: &Rc<TermWin>, slot: &str) -> bool {
        let Some(pane) = Self::at(window, slot) else {
            return false;
        };
        Self::forget(window, &pane);
        PaneClosure::remove(window, &pane.widget);
        true
    }

    /// Moves keyboard focus to one pane.
    pub(crate) fn focus(window: &Rc<TermWin>, slot: &str) -> bool {
        let Some(pane) = Self::at(window, slot) else {
            return false;
        };
        pane.content.grab_focus()
    }

    /// Sets how much of its split one pane takes.
    pub(crate) fn ratio(window: &Rc<TermWin>, slot: &str, ratio: f64) -> Adjustment {
        let Some(pane) = Self::at(window, slot) else {
            return Adjustment::Absent;
        };
        let Some(paned) = pane
            .widget
            .parent()
            .and_then(|parent| parent.downcast::<gtk::Paned>().ok())
        else {
            return Adjustment::Whole;
        };
        // The fraction names this pane's share, so the split's own position is
        // its complement when the pane is the second half.
        let held = ratio.clamp(0.05, 0.95);
        let leading = paned.start_child().as_ref() == Some(&pane.widget);
        let share = if leading { held } else { 1.0 - held };
        SplitPosition::restore(&paned, Self::direction(&paned), share);
        Adjustment::Set
    }

    /// Divides one pane, putting `content` in the half that appeared.
    pub(crate) fn divide(
        window: &Rc<TermWin>,
        slot: &str,
        orientation: gtk::Orientation,
        content: &gtk::Widget,
    ) -> bool {
        let Some(pane) = Self::at(window, slot) else {
            return false;
        };
        let content = if PaneChrome::is(content) {
            content.clone()
        } else {
            PaneChrome::wrap(window, content)
        };
        PaneSplit::insert(&pane.widget, orientation, &content)
    }

    /// Which way a split divides, in the layout's own words.
    fn direction(paned: &gtk::Paned) -> SplitDir {
        if paned.orientation() == gtk::Orientation::Horizontal {
            SplitDir::Horizontal
        } else {
            SplitDir::Vertical
        }
    }

    /// Drops one pane from whichever registry holds it.
    fn forget(window: &Rc<TermWin>, pane: &Occupancy) {
        let Some(terminal) = pane.content.downcast_ref::<vte4::Terminal>() else {
            // The interface goes back to its page rather than closing with the pane.
            Surface::retire(window, &pane.content);
            Slots::new(window).release(&pane.content);
            return;
        };
        Slots::new(window).discard(terminal);
    }

    /// One step of the walk: a registered pane is a leaf, anything else is a
    /// container to descend into.
    fn walk(window: &Rc<TermWin>, widget: &gtk::Widget, found: &mut Vec<Occupancy>) {
        if let Some(occupancy) = Self::occupancy(window, widget) {
            found.push(occupancy);
            return;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            Self::walk(window, &current, found);
            child = current.next_sibling();
        }
    }

    /// What this widget is, if it is a pane at all.
    ///
    /// A surface pane is a leaf even though it has children: the widgets under
    /// it are the extension's drawing, not panes of this window.
    fn occupancy(window: &Rc<TermWin>, widget: &gtk::Widget) -> Option<Occupancy> {
        if !PaneChrome::is(widget) {
            return None;
        }
        let content = PaneChrome::occupant(widget)?;
        if let Some(terminal) = content.downcast_ref::<vte4::Terminal>() {
            return Slots::new(window).of(terminal).map(|slot| Occupancy {
                widget: widget.clone(),
                content,
                slot,
                occupant: Occupant::Terminal,
            });
        }
        Slots::new(window).surface(&content).map(|(slot, _, _)| Occupancy {
            widget: widget.clone(),
            content,
            slot,
            occupant: Occupant::Surface,
        })
    }
}
