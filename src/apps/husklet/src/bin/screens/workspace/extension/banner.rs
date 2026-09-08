//! The strip that appears when the extension stops.

use std::rc::Rc;

use gtk::prelude::*;

use super::sink::{Signal, Sink};

/// A failure may contain a registry response, an image reference, and several
/// nested causes. The banner is a recovery surface, not a log viewer, so keep
/// enough context to identify the failure without letting hostile diagnostics
/// dictate the window's minimum width or retain an unbounded string in GTK.
const DIAGNOSTIC_LIMIT: usize = hl_extension::port::SEMANTIC_TEXT_LIMIT;

/// A hidden-by-default strip above the extension's surface.
///
/// It is a sibling of the surface rather than a replacement for it: the point
/// of the frozen state is that the last interface the extension drew stays on
/// screen, so the user can still read it after the extension is gone.
pub struct Banner {
    widget: gtk::Box,
    title: gtk::Label,
    reason: gtk::Label,
    retry: gtk::Button,
}

impl Banner {
    /// Builds the strip and wires its retry action to the sink.
    pub fn new(sink: Rc<dyn Sink>) -> Self {
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .accessible_role(gtk::AccessibleRole::Alert)
            .build();
        widget.add_css_class("hl-extension-banner");
        widget.set_visible(false);

        let title = gtk::Label::new(Some("Extension unavailable"));
        title.add_css_class("hl-extension-banner-title");
        title.set_accessible_role(gtk::AccessibleRole::Heading);
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        widget.append(&title);

        let reason = gtk::Label::new(None);
        reason.add_css_class("hl-extension-banner-detail");
        reason.set_xalign(0.0);
        reason.set_hexpand(true);
        reason.set_wrap(true);
        reason.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        reason.set_max_width_chars(64);
        reason.set_selectable(true);
        widget.append(&reason);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("hl-extension-retry");
        retry.set_halign(gtk::Align::Start);
        retry.update_property(&[gtk::accessible::Property::Label("Retry extension")]);
        retry.connect_clicked(move |button| {
            if button.is_sensitive() {
                button.set_sensitive(false);
                sink.accept(Signal::Retry);
            }
        });
        widget.append(&retry);
        widget.update_relation(&[
            gtk::accessible::Relation::LabelledBy(&[title.upcast_ref()]),
            gtk::accessible::Relation::DescribedBy(&[reason.upcast_ref()]),
        ]);
        Self {
            widget,
            title,
            reason,
            retry,
        }
    }

    /// The strip, for placing above the surface.
    #[must_use]
    pub const fn widget(&self) -> &gtk::Box {
        &self.widget
    }

    /// Shows the strip and says why the extension stopped.
    pub fn show(&self, reason: &str) {
        self.reason.set_text(&bounded_diagnostic(reason));
        self.retry.set_sensitive(true);
        self.widget.set_visible(true);
    }

    /// Hides the strip, which is what a fresh frame means.
    pub fn hide(&self) {
        self.retry.set_sensitive(true);
        self.widget.set_visible(false);
    }

    /// Marks the one recovery request as in flight until a fresh frame arrives.
    pub fn pending(&self) {
        self.retry.set_sensitive(false);
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

    /// The stable alert heading, for semantic and accessibility assertions.
    #[must_use]
    pub fn title(&self) -> String {
        self.title.text().to_string()
    }
}

/// Produces the compact diagnostic shown beneath the stable recovery heading.
fn bounded_diagnostic(reason: &str) -> String {
    let mut diagnostic: String = reason.trim().chars().take(DIAGNOSTIC_LIMIT + 1).collect();
    if diagnostic.chars().count() > DIAGNOSTIC_LIMIT {
        diagnostic.pop();
        diagnostic.pop();
        diagnostic.push('…');
    }
    if diagnostic.is_empty() {
        "The extension stopped without a diagnostic.".to_owned()
    } else {
        diagnostic
    }
}
