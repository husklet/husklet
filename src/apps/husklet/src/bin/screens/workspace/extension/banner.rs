//! The strip that appears when the extension stops.

use std::rc::Rc;

use gtk::prelude::*;

use super::sink::{Signal, Sink};

/// A failure may contain a registry response, an image reference, and several
/// nested causes. The banner is a recovery surface, not a log viewer, so keep
/// enough context to identify the failure without letting hostile diagnostics
/// dictate the window's minimum width or retain an unbounded string in GTK.
const DIAGNOSTIC_LIMIT: usize = 16 * 1024;

/// A hidden-by-default strip above the extension's surface.
///
/// It is a sibling of the surface rather than a replacement for it: the point
/// of the frozen state is that the last interface the extension drew stays on
/// screen, so the user can still read it after the extension is gone.
pub struct Banner {
    widget: gtk::Box,
    title: gtk::Label,
    summary: gtk::Label,
    reason: gtk::Label,
    retry: gtk::Button,
    copied: gtk::Label,
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

        let summary = gtk::Label::new(Some("The extension connection stopped. No change was assumed."));
        summary.set_xalign(0.0);
        summary.set_wrap(true);
        summary.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        widget.append(&summary);

        let reason = gtk::Label::new(None);
        reason.add_css_class("hl-extension-banner-detail");
        reason.set_xalign(0.0);
        reason.set_hexpand(true);
        reason.set_wrap(true);
        reason.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        reason.set_max_width_chars(64);
        reason.set_selectable(true);
        let details = gtk::Expander::new(Some("Technical details"));
        details.set_child(Some(&reason));
        details.set_hexpand(true);
        widget.append(&details);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
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
        actions.append(&retry);
        let copy = gtk::Button::with_label("Copy");
        copy.add_css_class("hl-extension-copy");
        copy.set_tooltip_text(Some("Copy the complete diagnostic for support or debugging"));
        copy.update_property(&[gtk::accessible::Property::Label("Copy technical details")]);
        let copied = gtk::Label::new(Some("Copied"));
        copied.add_css_class("hl-extension-copied");
        copied.set_visible(false);
        let detail = reason.clone();
        let confirmation = copied.clone();
        copy.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&detail.text());
                confirmation.set_visible(true);
            }
        });
        actions.append(&copy);
        actions.append(&copied);
        widget.append(&actions);
        widget.update_relation(&[
            gtk::accessible::Relation::LabelledBy(&[title.upcast_ref()]),
            gtk::accessible::Relation::DescribedBy(&[summary.upcast_ref()]),
        ]);
        Self {
            widget,
            title,
            summary,
            reason,
            retry,
            copied,
        }
    }

    /// The strip, for placing above the surface.
    #[must_use]
    pub const fn widget(&self) -> &gtk::Box {
        &self.widget
    }

    /// Shows the strip and says why the extension stopped.
    pub fn show(&self, reason: &str) {
        self.summary.set_text(recovery_message(reason));
        self.reason.set_text(&bounded_diagnostic(reason));
        self.copied.set_visible(false);
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

fn recovery_message(reason: &str) -> &'static str {
    let normalized = reason.to_ascii_lowercase();
    if normalized.contains("expected frame") || normalized.contains("received frame") {
        "The extension sent an inconsistent update. Retry to reconnect without applying it."
    } else if normalized.contains("unknown variant")
        || normalized.contains("record state/extensions/")
        || normalized.contains("protocol") && normalized.contains("speaks")
    {
        "This installation is incompatible with the current Husklet build. Reinstall it from Extensions."
    } else if normalized.contains("permission denied")
        || normalized.contains("not granted")
        || normalized.contains("capability") && normalized.contains("required")
    {
        "This extension needs access it does not currently have. Review its permissions in Extensions."
    } else if normalized.contains("http 409")
        || normalized.contains("expected running")
        || normalized.contains("nativefailed")
        || normalized.contains("nativerunfailed")
    {
        "The workspace runtime stopped before the extension connected. Restart the workspace, then retry."
    } else if normalized.contains("image")
        && (normalized.contains("not found") || normalized.contains("manifest") || normalized.contains("pull"))
    {
        "The extension image is unavailable or invalid. Check its source and reinstall it."
    } else if normalized.contains("timed out") || normalized.contains("timeout") {
        "The extension did not respond in time. Retry the connection."
    } else {
        "The extension connection stopped. Retry to reconnect; the last successful view remains below."
    }
}

/// Produces the compact diagnostic shown beneath the stable recovery heading.
fn bounded_diagnostic(reason: &str) -> String {
    let reason = reason.trim();
    let truncated = reason.chars().count() > DIAGNOSTIC_LIMIT;
    let mut diagnostic: String = reason.chars().take(DIAGNOSTIC_LIMIT).collect();
    if truncated {
        diagnostic.pop();
        diagnostic.push('…');
    }
    if diagnostic.is_empty() {
        "The extension stopped without a diagnostic.".to_owned()
    } else {
        diagnostic
    }
}

#[cfg(test)]
mod tests {
    use super::{DIAGNOSTIC_LIMIT, bounded_diagnostic, recovery_message};

    #[test]
    fn frame_numbers_never_lead_the_recovery_surface() {
        let raw = "expected frame 8, received frame 10";
        assert_eq!(
            recovery_message(raw),
            "The extension sent an inconsistent update. Retry to reconnect without applying it."
        );
        assert_eq!(bounded_diagnostic(raw), raw);
    }

    #[test]
    fn technical_diagnostics_are_bounded() {
        assert!(bounded_diagnostic(&"x".repeat(DIAGNOSTIC_LIMIT * 2)).chars().count() <= DIAGNOSTIC_LIMIT);
    }

    #[test]
    fn raw_runtime_and_state_failures_become_actionable_summaries() {
        assert_eq!(
            recovery_message("extension record state/extensions/top is unreadable: unknown variant `old:scope`"),
            "This installation is incompatible with the current Husklet build. Reinstall it from Extensions."
        );
        assert_eq!(
            recovery_message("Docker API returned HTTP 409 Conflict: container is Exited, expected running"),
            "The workspace runtime stopped before the extension connected. Restart the workspace, then retry."
        );
        assert_eq!(
            recovery_message("capability containers:write is required but not granted"),
            "This extension needs access it does not currently have. Review its permissions in Extensions."
        );
    }
}
