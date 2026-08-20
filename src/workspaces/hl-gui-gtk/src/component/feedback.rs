//! Progress, emptiness, figures and messages.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Height a skeleton stands in for until real content arrives, in pixels.
const PLACEHOLDER_PIXELS: i32 = 16;

/// Components that report state rather than accept it.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Progress => progress().upcast(),
        Tag::Spinner => spinner().upcast(),
        Tag::Meter => gtk::LevelBar::new().upcast(),
        Tag::Skeleton => skeleton().upcast(),
        Tag::EmptyState => vacancy().upcast(),
        Tag::Stat => figure().upcast(),
        Tag::AlertTitle => title().upcast(),
        Tag::InlineMessage => strip().upcast(),
        // Toast and Banner are the last feedback tags routed here. Both live in
        // libadwaita; a revealer over an icon and a message gives the behavior
        // — appear, carry text, dismiss — without taking that dependency.
        _ => notice().upcast(),
    }
}

fn progress() -> gtk::ProgressBar {
    let widget = gtk::ProgressBar::new();
    widget.set_hexpand(true);
    widget
}

fn spinner() -> gtk::Spinner {
    let widget = gtk::Spinner::new();
    widget.start();
    widget
}

/// A blank of the size the awaited content will occupy. It carries no text on
/// purpose: what it says is that something is coming, and how large it is.
fn skeleton() -> gtk::Box {
    let widget = axis::row(0);
    widget.set_size_request(-1, PLACEHOLDER_PIXELS);
    widget.set_hexpand(true);
    widget
}

/// What to show where there is nothing: an icon, a line of explanation, and
/// room for the action that resolves it.
fn vacancy() -> gtk::Box {
    let widget = axis::column(8);
    widget.set_valign(gtk::Align::Center);
    widget.set_halign(gtk::Align::Center);
    widget.append(&slot::emblem_image());
    widget.append(&slot::caption_label());
    widget.append(&slot::detail_label());
    widget
}

/// One measured figure over its caption. The figure is the value slot, so a
/// producer sends the number as `Value` and the name as `Label`.
fn figure() -> gtk::Box {
    let widget = axis::column(2);
    let measure = axis::label();
    measure.add_css_class("title-1");
    slot::field(&measure);
    widget.append(&measure);
    widget.append(&slot::caption_label());
    widget
}

fn title() -> gtk::Label {
    let widget = axis::label();
    widget.add_css_class("title-4");
    widget
}

/// A message that stands beside what it is about, without dismissal.
fn strip() -> gtk::Box {
    let widget = axis::row(8);
    widget.append(&slot::emblem_image());
    widget.append(&slot::caption_label());
    widget
}

/// The message surface behind Toast and Banner: an icon and a message column,
/// both built up front so `Icon` and `Label` have somewhere to land.
fn notice() -> gtk::Revealer {
    let strip = axis::row(8);
    let column = axis::column(2);
    column.set_hexpand(true);
    column.append(&slot::caption_label());
    strip.append(&slot::emblem_image());
    strip.append(&column);
    let widget = gtk::Revealer::new();
    widget.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    widget.set_reveal_child(true);
    widget.set_child(Some(&strip));
    widget
}

/// The message column of a notice: everything described inside a message
/// belongs under its text, not beside its icon.
fn column(revealer: &gtk::Revealer) -> Option<gtk::Box> {
    revealer
        .child()
        .and_then(|strip| strip.last_child())
        .and_then(|column| column.downcast::<gtk::Box>().ok())
}

/// Attaches to a notice, placing a heading above the message it heads.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    let Some(revealer) = parent.downcast_ref::<gtk::Revealer>() else {
        return false;
    };
    let Some(column) = column(revealer) else {
        revealer.set_child(Some(child));
        return true;
    };
    match tag {
        Tag::AlertTitle => column.prepend(child),
        _ => column.append(child),
    }
    true
}
