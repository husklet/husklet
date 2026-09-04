//! Fields: the components a value is typed, picked or dragged into.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Hours in a day and minutes in an hour, as the bounds of the two counters a
/// time is entered with.
const HOURS: f64 = 23.0;
const MINUTES: f64 = 59.0;
/// Stars a rating is measured in.
const STARS: u8 = 5;

/// Components that hold a value.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Entry => entry().upcast(),
        Tag::Search => gtk::SearchEntry::new().upcast(),
        Tag::CommandPalette => command_palette().upcast(),
        Tag::TagInput => tag_input().upcast(),
        Tag::NumberEntry => counter(0.0, 100.0).upcast(),
        Tag::TextArea => editor(true).upcast(),
        Tag::PasswordEntry => secret().upcast(),
        Tag::Autocomplete => completion().upcast(),
        Tag::TextField => field().upcast(),
        Tag::InputAdornment => adornment().upcast(),
        Tag::Slider => slider().upcast(),
        Tag::Rating => rating().upcast(),
        Tag::DatePicker => gtk::Calendar::new().upcast(),
        Tag::TimePicker => clock().upcast(),
        // ColorPicker is the last field tag routed here.
        _ => gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new())).upcast(),
    }
}

pub(crate) fn set_color(widget: &gtk::Widget, value: &str) -> bool {
    let Some(picker) = widget.downcast_ref::<gtk::ColorDialogButton>() else { return false };
    let Ok(color) = gtk::gdk::RGBA::parse(value) else { return true };
    picker.set_rgba(&color);
    true
}

pub(crate) fn color_value(color: &gtk::gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.red().clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.green().clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.blue().clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn command_palette() -> gtk::Box {
    let widget = axis::column(4);
    let search = gtk::SearchEntry::new();
    search.set_search_delay(100);
    search.set_hexpand(true);
    slot::field(&search);
    widget.append(&search);
    widget
}

fn tag_input() -> gtk::Box {
    let widget = axis::row(4);
    widget.add_css_class("linked");
    let entry = entry();
    entry.set_hexpand(true);
    widget.append(&entry);
    widget
}

fn entry() -> gtk::Entry {
    let widget = gtk::Entry::new();
    slot::field(&widget);
    widget
}

fn secret() -> gtk::PasswordEntry {
    let widget = gtk::PasswordEntry::new();
    // The reveal control is the difference between this and a hidden entry:
    // a person can check what they typed before committing to it.
    widget.set_show_peek_icon(true);
    slot::field(&widget);
    widget
}

fn counter(lower: f64, upper: f64) -> gtk::SpinButton {
    let adjustment = gtk::Adjustment::new(lower, lower, upper, 1.0, 10.0, 0.0);
    gtk::SpinButton::new(Some(&adjustment), 1.0, 0)
}

/// A multi-line editor is a view plus its scroller; the pair is the component.
pub(crate) fn editor(editable: bool) -> gtk::ScrolledWindow {
    let view = gtk::TextView::new();
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_monospace(true);
    view.set_editable(editable);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&view));
    window.set_min_content_height(96);
    window.set_hexpand(true);
    window
}

/// Completion over a fixed candidate list.
///
/// GTK4 deprecated free-text completion with `GtkEntryCompletion`, so this is
/// a searchable drop-down: what is offered is the declared choices, and a
/// person narrows them by typing. It does not accept a value outside them.
fn completion() -> gtk::DropDown {
    let widget = gtk::DropDown::from_strings(&[]);
    widget.set_enable_search(true);
    widget.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
    widget
}

/// A field with its name above it and room for an explanation below.
fn field() -> gtk::Box {
    let widget = axis::column(4);
    let line = axis::row(4);
    line.append(&entry());
    widget.append(&slot::caption_label());
    widget.append(&line);
    widget.append(&slot::detail_label());
    widget
}

fn adornment() -> gtk::Box {
    let widget = axis::row(2);
    widget.set_valign(gtk::Align::Center);
    widget.append(&slot::caption_label());
    widget
}

fn slider() -> gtk::Scale {
    let widget = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    widget.set_hexpand(true);
    widget.set_draw_value(true);
    widget
}

/// A rating, in whole stars.
///
/// GTK4 has no rating widget. A box of toggle buttons would be inert here: a
/// value reaches a component through the widget its node was created for, and
/// only that widget reports interaction — a button built inside a composite
/// could be pressed and would tell the producer nothing. A scale marked in
/// stars is the control that actually works: it holds `Value`, moves in whole
/// stars, and reports `Change`.
fn rating() -> gtk::Scale {
    let widget = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, f64::from(STARS), 1.0);
    widget.set_round_digits(0);
    widget.set_draw_value(false);
    for star in 1..=STARS {
        widget.add_mark(f64::from(star), gtk::PositionType::Bottom, Some("★"));
    }
    widget
}

/// An hour counter and a minute counter. GTK4 has no time widget, and two
/// bounded counters are the honest equivalent: every value they can show is a
/// real time, which a free-text field cannot promise.
fn clock() -> gtk::Box {
    let widget = axis::row(4);
    widget.append(&counter(0.0, HOURS));
    widget.append(&gtk::Label::new(Some(":")));
    widget.append(&counter(0.0, MINUTES));
    widget
}

/// The two counters of a time field, in order.
pub(crate) fn counters(widget: &gtk::Widget) -> Option<(gtk::SpinButton, gtk::SpinButton)> {
    let children = slot::offspring(widget);
    let hours = children.first()?.clone().downcast::<gtk::SpinButton>().ok()?;
    let minutes = children.last()?.clone().downcast::<gtk::SpinButton>().ok()?;
    Some((hours, minutes))
}

/// The text view behind an editor component, when the widget is one.
pub(crate) fn view(widget: &gtk::Widget) -> Option<gtk::TextView> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::TextView>().ok())
}

/// Places an adornment beside the value it decorates rather than under it.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag, index: usize) -> bool {
    if super::belongs(parent, Tag::TagInput) {
        let Some(container) = parent.downcast_ref::<gtk::Box>() else {
            return false;
        };
        let entry = slot::editable(parent);
        container.insert_child_after(child, entry.as_ref().and_then(gtk::Widget::prev_sibling).as_ref());
        return true;
    }
    if tag != Tag::InputAdornment {
        return false;
    }
    let Some(line) = slot::editable(parent).and_then(|held| held.parent()) else {
        return false;
    };
    let Some(line) = line.downcast_ref::<gtk::Box>() else {
        return false;
    };
    // Before the value or after it: an adornment described first is a prefix.
    match index {
        0 => line.prepend(child),
        _ => line.append(child),
    }
    true
}
