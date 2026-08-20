//! The strip that appears when the extension stops.

use std::rc::Rc;

use gtk::prelude::*;

use super::sink::{Signal, Sink};

/// A hidden-by-default strip above the extension's surface.
///
/// It is a sibling of the surface rather than a replacement for it: the point
/// of the frozen state is that the last interface the extension drew stays on
/// screen, so the user can still read it after the extension is gone.
pub struct Banner {
    widget: gtk::Box,
    reason: gtk::Label,
}

impl Banner {
    /// Builds the strip and wires its retry action to the sink.
    pub fn new(sink: Rc<dyn Sink>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        widget.add_css_class("hl-extension-banner");
        widget.set_visible(false);
        let reason = gtk::Label::new(None);
        reason.set_xalign(0.0);
        reason.set_hexpand(true);
        reason.set_wrap(true);
        widget.append(&reason);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("hl-extension-retry");
        retry.connect_clicked(move |_| sink.accept(Signal::Retry));
        widget.append(&retry);
        Self { widget, reason }
    }

    /// The strip, for placing above the surface.
    #[must_use]
    pub const fn widget(&self) -> &gtk::Box {
        &self.widget
    }

    /// Shows the strip and says why the extension stopped.
    pub fn show(&self, reason: &str) {
        self.reason.set_text(&format!("The extension stopped: {reason}"));
        self.widget.set_visible(true);
    }

    /// Hides the strip, which is what a fresh frame means.
    pub fn hide(&self) {
        self.widget.set_visible(false);
    }

    /// Whether the strip is currently shown.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.widget.is_visible()
    }

    /// What the strip currently says, for tests and diagnostics.
    #[must_use]
    pub fn text(&self) -> String {
        self.reason.text().to_string()
    }
}
