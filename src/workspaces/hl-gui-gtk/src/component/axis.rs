//! The two box orientations, built the way every family here wants them.

use gtk::prelude::*;

/// A vertical box with the given spacing.
pub(crate) fn column(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// A horizontal box with the given spacing.
pub(crate) fn row(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

/// A label that reads from its start edge, which is what body text wants.
pub(crate) fn label() -> gtk::Label {
    let widget = gtk::Label::new(None);
    widget.set_halign(gtk::Align::Start);
    widget.set_xalign(0.0);
    widget
}

/// A button drawn without its frame, for the many places a button is a row or
/// an item rather than a raised control.
pub(crate) fn item() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_has_frame(false);
    widget
}
