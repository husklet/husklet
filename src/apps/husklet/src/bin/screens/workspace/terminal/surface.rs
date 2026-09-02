//! A pane that holds an extension's interface instead of a shell.
//!
//! What is persisted about such a pane is its place in the layout and the name
//! of the extension whose interface belongs in it — never the drawing. An
//! interface is a stream of reconciliation frames from a running extension, and
//! there is nothing here that could hold or replay one. A restored pane whose
//! extension is not drawing therefore says so, and stays a pane of that
//! extension; it never turns into a shell, because a person who left an
//! interface there did not ask for one.

use super::*;

/// The style class every pane holding an interface carries.
pub(crate) const SURFACE: &str = "hl-surface";

/// The style class the strip of an undrawn surface carries.
pub(crate) const ABSENCE: &str = "hl-surface-absence";

/// A pane an extension draws into.
pub(crate) struct Surface;

impl Surface {
    /// Legacy single-interface lookup, used only by test/fallback galleries
    /// that have not registered independent pane renderers.
    pub(crate) fn of(window: &Rc<TermWin>, extension: &str) -> Option<gtk::Widget> {
        Panes::all(window).into_iter().find_map(|pane| {
            Slots::new(window)
                .surface(&pane.content)
                .is_some_and(|(_, held, _)| held == extension)
                .then_some(pane.content)
        })
    }

    /// Builds the pane for one extension and registers it under `slot`.
    ///
    /// The interface placed in it is retained under this stable slot. Other
    /// panes from the same extension own different widgets and trees.
    pub(crate) fn build(window: &Rc<TermWin>, extension: &str, provider: Option<&str>, slot: String) -> gtk::Widget {
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.add_css_class(SURFACE);
        holder.set_hexpand(true);
        holder.set_vexpand(true);
        holder.set_focusable(true);
        match Window::gallery(window).and_then(|gallery| gallery.pane(extension, &slot)) {
            Some(interface) => holder.append(&interface),
            None => holder.append(&Absence::widget(extension)),
        }
        let widget: gtk::Widget = holder.upcast();
        Slots::new(window).enrol(&widget, slot, extension.to_owned(), provider.map(str::to_owned));
        widget
    }

    /// Gives an interface back to its page on the workspace shell.
    ///
    /// A closing pane must not take the extension's only interface with it: the
    /// widget would be dropped with the pane, and the extension would go on
    /// describing an interface with nothing left to apply it to.
    pub(crate) fn retire(window: &Rc<TermWin>, pane: &gtk::Widget) {
        let Some((_, extension, _)) = Slots::new(window).surface(pane) else {
            return;
        };
        let Some(holder) = pane.downcast_ref::<gtk::Box>() else {
            return;
        };
        let Some(child) = holder.first_child() else { return };
        holder.remove(&child);
        // Addressed pane interfaces remain retained by their extension page.
        // They are deliberately left detached until this same slot is mounted
        // again; the overview owns its own independent interface.
        if let Some(gallery) = Window::gallery(window) {
            gallery.release(&extension, &child);
        }
    }

    /// Gives up a pane that never reached the layout: its interface goes home
    /// and its slot is forgotten, so nothing is addressable that is not there.
    pub(crate) fn discard(window: &Rc<TermWin>, pane: &gtk::Widget) {
        Self::retire(window, pane);
        Slots::new(window).release(pane);
    }

    /// Moves the recorded interface into an existing surface holder.
    ///
    /// Used only to roll back a division that failed after [`Self::build`] had
    /// borrowed the interface. The holder remains registered throughout, so
    /// its slot never disappears or changes occupant while the move is tried.
    pub(crate) fn restore(window: &Rc<TermWin>, extension: &str, pane: &gtk::Widget) {
        let Some(holder) = pane.downcast_ref::<gtk::Box>() else {
            return;
        };
        let Some((slot, _, _)) = Slots::new(window).surface(pane) else {
            return;
        };
        let Some(interface) = Window::gallery(window).and_then(|gallery| gallery.pane(extension, &slot)) else {
            return;
        };
        while let Some(child) = holder.first_child() {
            holder.remove(&child);
        }
        holder.append(&interface);
    }

    /// Replaces a withdrawn interface with an explicit placeholder while
    /// retaining the surface registration and any displaced terminal.
    pub(crate) fn tombstone(window: &Rc<TermWin>, pane: &gtk::Widget) {
        let Some((_, extension, _)) = Slots::new(window).surface(pane) else {
            return;
        };
        let Some(holder) = pane.downcast_ref::<gtk::Box>() else {
            return;
        };
        if let Some(child) = holder.first_child() {
            holder.remove(&child);
            if !child.has_css_class(ABSENCE) {
                if let Some(gallery) = Window::gallery(window) {
                    gallery.recover(&extension, &child);
                }
            }
        }
        holder.append(&Absence::widget(&extension));
    }
}

/// The pane of an extension that is not drawing into it.
pub(crate) struct Absence;

impl Absence {
    /// A strip saying whose interface belongs in this pane and that nobody is
    /// drawing it, which is the same thing a stopped extension's page says.
    pub(crate) fn widget(extension: &str) -> gtk::Box {
        let strip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        strip.add_css_class(ABSENCE);
        strip.set_hexpand(true);
        strip.set_vexpand(true);
        let label = gtk::Label::new(Some(&format!(
            "The extension {extension} is not drawing here: its interface is restored only while it runs."
        )));
        label.set_wrap(true);
        label.set_xalign(0.0);
        strip.append(&label);
        strip
    }
}
