//! Where each extension's interface widget is, so a pane can hold the one that
//! already exists.
//!
//! An extension draws one interface: a stream of reconciliation frames applied
//! to one tree. When an extension asks for a pane to draw into, the interface
//! it is already drawing is what goes in the pane, and its page on the
//! workspace shell keeps the empty holder it came out of until it comes back.
//! Anything else would be a second tree that never received the frames the
//! first was built from.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// One extension's interface and the page it belongs to.
struct Exhibit {
    /// The widget the extension's frames are applied to.
    interface: glib::WeakRef<gtk::Widget>,
    /// The holder on the workspace shell it was placed in, which is where it
    /// goes back to when a pane holding it closes.
    home: glib::WeakRef<gtk::Box>,
}

/// Every extension's interface, by name.
///
/// Held weakly throughout: a shell that closed takes its pages with it, and a
/// gallery is a place to look something up, not a reason for a widget to
/// outlive the window it was drawn in.
#[derive(Clone, Default)]
pub struct Gallery(Rc<RefCell<HashMap<String, Exhibit>>>);

impl Gallery {
    /// An empty gallery.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records where one extension's interface is, replacing whatever was
    /// recorded under that name — a rebuilt page is a new interface.
    pub fn enrol(&self, extension: &str, interface: &impl IsA<gtk::Widget>, home: &gtk::Box) {
        let exhibit = Exhibit {
            interface: interface.as_ref().downgrade(),
            home: home.downgrade(),
        };
        self.0.borrow_mut().insert(extension.to_owned(), exhibit);
    }

    /// Takes one extension's interface out of its page, for a pane to hold.
    ///
    /// `None` when nothing is recorded under that name or the page has gone,
    /// which is what an extension that is not installed here looks like.
    #[must_use]
    pub fn lend(&self, extension: &str) -> Option<gtk::Widget> {
        let held = self.0.borrow();
        let exhibit = held.get(extension)?;
        let interface = exhibit.interface.upgrade()?;
        if let Some(parent) = interface.parent().and_downcast::<gtk::Box>() {
            parent.remove(&interface);
        }
        Some(interface)
    }

    /// Puts one extension's interface back on its page.
    pub fn recover(&self, extension: &str, interface: &gtk::Widget) {
        let held = self.0.borrow();
        let Some(home) = held.get(extension).and_then(|exhibit| exhibit.home.upgrade()) else {
            return;
        };
        if interface.parent().is_none() {
            home.append(interface);
        }
    }

    /// Whether an interface is recorded under this name and still drawable.
    #[must_use]
    pub fn holds(&self, extension: &str) -> bool {
        self.0
            .borrow()
            .get(extension)
            .is_some_and(|exhibit| exhibit.interface.upgrade().is_some())
    }
}

impl std::fmt::Debug for Gallery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Gallery")
            .field("exhibits", &self.0.borrow().len())
            .finish_non_exhaustive()
    }
}
