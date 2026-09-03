//! Where a node's own text, icon and reference land.
//!
//! A component is often several widgets, and its `Label` and its `Value` are
//! different things: a field's label names it while its value is what it holds,
//! and a statistic's label is the caption under the figure. Each property is
//! therefore placed in the widget that owns that meaning, falling back to the
//! other only when the component has just one place to put text — which is what
//! keeps a `Text` node honouring both.

use gtk::prelude::*;
use hl_gui::{PropValue, Tag};

use crate::component::{content, field, slot};

/// Places what a node is called.
pub(crate) fn caption(widget: &gtk::Widget, tag: Tag, value: &PropValue) {
    if mark(widget, tag, value) {
        return;
    }
    hold(widget, tag, value);
}

/// Places what a node holds.
pub(crate) fn body(widget: &gtk::Widget, tag: Tag, value: &PropValue) {
    if hold(widget, tag, value) {
        return;
    }
    mark(widget, tag, value);
}

/// Places the secondary text beside a node's own, or keeps it as the
/// explanation a pointer reveals when the component has no room for it.
pub(crate) fn detail(widget: &gtk::Widget, value: &PropValue) {
    let Some(label) = slot::detail(widget) else {
        widget.set_tooltip_text(value.as_text());
        return;
    };
    label.set_text(value.as_text().unwrap_or_default());
}

/// The widgets whose own text is their caption.
fn mark(widget: &gtk::Widget, tag: Tag, value: &PropValue) -> bool {
    let content = value.as_text().unwrap_or_default();
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_text(content);
        return true;
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        button.set_label(content);
        return true;
    }
    // A check button and a menu button are not button subclasses in GTK4, so
    // each needs its own arm.
    if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
        check.set_label(Some(content));
        return true;
    }
    if let Some(menu) = widget.downcast_ref::<gtk::MenuButton>() {
        menu.set_label(content);
        return true;
    }
    framed(widget, tag, content)
}

/// The widgets that draw their caption in their own chrome, and the composites
/// that keep a slot for it.
fn framed(widget: &gtk::Widget, tag: Tag, content: &str) -> bool {
    if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
        expander.set_label(Some(content));
        return true;
    }
    // A frame carries its title in its own header, above whatever it holds.
    if let Some(frame) = widget.downcast_ref::<gtk::Frame>() {
        frame.set_label(Some(content));
        return true;
    }
    if let Some(caption) = slot::caption(widget) {
        caption.set_text(content);
        return true;
    }
    plotted(widget, tag, content)
}

/// A drawing area holds no text, so a chart's label is kept as its tooltip and
/// painted from there by the draw function.
fn plotted(widget: &gtk::Widget, tag: Tag, content: &str) -> bool {
    if tag != Tag::Chart {
        return false;
    }
    let Some(area) = widget.downcast_ref::<gtk::DrawingArea>() else {
        return false;
    };
    area.set_tooltip_text(Some(content));
    area.queue_draw();
    true
}

/// The widgets that hold a value of their own.
fn hold(widget: &gtk::Widget, tag: Tag, value: &PropValue) -> bool {
    let content = value.as_text().unwrap_or_default();
    if tag == Tag::Sparkline {
        return content::samples(widget, content);
    }
    if tag == Tag::FlameGraph {
        return content::flames(widget, content);
    }
    if tag == Tag::MemoryMap {
        return content::regions(widget, content);
    }
    if tag == Tag::DisassemblyView {
        return content::instructions(widget, content);
    }
    if tag == Tag::TimelineView {
        return content::timeline(widget, content);
    }
    if tag == Tag::TestReportView {
        return content::test_report(widget, content);
    }
    if tag == Tag::CoverageView {
        return content::coverage(widget, content);
    }
    if matches!(tag, Tag::NetworkRequest | Tag::NetworkPhase) {
        return content::network_value(widget, tag, content);
    }
    if tag == Tag::MarkdownView {
        return content::markdown(widget, content);
    }
    if tag == Tag::JsonView {
        return content::json(widget, content);
    }
    if tag == Tag::LogView {
        return content::append(widget, content);
    }
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.set_text(content);
        return true;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>() {
        entry.set_text(content);
        return true;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::PasswordEntry>() {
        entry.set_text(content);
        return true;
    }
    measured(widget, tag, value)
}

/// The widgets whose value is a number, a date or a time.
fn measured(widget: &gtk::Widget, tag: Tag, value: &PropValue) -> bool {
    if let Some(view) = field::view(widget) {
        view.buffer().set_text(value.as_text().unwrap_or_default());
        return true;
    }
    if let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() {
        spin.set_value(value.as_number().unwrap_or_default());
        return true;
    }
    if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        scale.set_value(value.as_number().unwrap_or_default());
        return true;
    }
    if let Some(level) = widget.downcast_ref::<gtk::LevelBar>() {
        level.set_value(value.as_number().unwrap_or_default());
        return true;
    }
    calendared(widget, tag, value)
}

fn calendared(widget: &gtk::Widget, tag: Tag, value: &PropValue) -> bool {
    let content = value.as_text().unwrap_or_default();
    if let Some(calendar) = widget.downcast_ref::<gtk::Calendar>() {
        return day(calendar, content);
    }
    if tag == Tag::TimePicker {
        return time(widget, content);
    }
    inner(widget, tag, value)
}

/// A composite keeps its value in a slot of its own, which may be an editable
/// or — for a figure that is read rather than typed — a label.
fn inner(widget: &gtk::Widget, tag: Tag, value: &PropValue) -> bool {
    let Some(held) = slot::editable(widget) else {
        return false;
    };
    if let Some(label) = held.downcast_ref::<gtk::Label>() {
        label.set_text(value.as_text().unwrap_or_default());
        return true;
    }
    hold(&held, tag, value)
}

/// Selects a day written as an ISO 8601 date.
///
/// The wire carries text, and a calendar needs three numbers; ISO 8601 is the
/// one spelling of a date that is not an invention of this library.
fn day(calendar: &gtk::Calendar, content: &str) -> bool {
    let mut parts = content.split('-').filter_map(|part| part.parse::<i32>().ok());
    let (Some(year), Some(month), Some(day)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let zone = gtk::glib::TimeZone::local();
    let Ok(moment) = gtk::glib::DateTime::new(&zone, year, month, day, 0, 0, 0.0) else {
        return false;
    };
    calendar.select_day(&moment);
    true
}

/// Sets the hour and minute counters from an ISO 8601 time of day.
fn time(widget: &gtk::Widget, content: &str) -> bool {
    let Some((hours, minutes)) = field::counters(widget) else {
        return false;
    };
    let mut parts = content.split(':').filter_map(|part| part.parse::<f64>().ok());
    let (Some(hour), Some(minute)) = (parts.next(), parts.next()) else {
        return false;
    };
    hours.set_value(hour);
    minutes.set_value(minute);
    true
}

/// Places the text a field shows while it holds nothing.
pub(crate) fn placeholder(widget: &gtk::Widget, value: &PropValue) {
    let held = slot::editable(widget);
    let target = held.as_ref().unwrap_or(widget);
    if let Some(entry) = target.downcast_ref::<gtk::Entry>() {
        entry.set_placeholder_text(value.as_text());
        return;
    }
    if let Some(entry) = target.downcast_ref::<gtk::SearchEntry>() {
        entry.set_placeholder_text(value.as_text());
        return;
    }
    if let Some(entry) = target.downcast_ref::<gtk::PasswordEntry>() {
        entry.set_placeholder_text(value.as_text());
    }
}

/// Places the named icon a node shows.
pub(crate) fn icon(widget: &gtk::Widget, value: &PropValue) {
    let name = value.as_text();
    if let Some(image) = widget.downcast_ref::<gtk::Image>() {
        image.set_icon_name(name);
        return;
    }
    if let Some(emblem) = slot::emblem(widget) {
        emblem.set_icon_name(name);
        return;
    }
    if let Some(menu) = widget.downcast_ref::<gtk::MenuButton>() {
        menu.set_icon_name(name.unwrap_or_default());
        return;
    }
    lettered(widget, name);
}

fn lettered(widget: &gtk::Widget, name: Option<&str>) {
    let Some(button) = widget.downcast_ref::<gtk::Button>() else {
        return;
    };
    let Some(name) = name else {
        return;
    };
    button.set_icon_name(name);
}

/// Places the file or address a node refers to.
pub(crate) fn uri(widget: &gtk::Widget, value: &PropValue) {
    if let Some(link) = widget.downcast_ref::<gtk::LinkButton>() {
        link.set_uri(value.as_text().unwrap_or_default());
        return;
    }
    if let Some(picture) = widget.downcast_ref::<gtk::Picture>() {
        picture.set_file(value.as_text().map(source).as_ref());
        return;
    }
    if let Some(video) = widget.downcast_ref::<gtk::Video>() {
        video.set_file(value.as_text().map(source).as_ref());
    }
}

/// An absolute path and a `scheme://` reference both name a file, and only the
/// second is a URI — reading a path as one silently loads nothing.
fn source(value: &str) -> gtk::gio::File {
    if value.contains("://") {
        return gtk::gio::File::for_uri(value);
    }
    gtk::gio::File::for_path(value)
}
