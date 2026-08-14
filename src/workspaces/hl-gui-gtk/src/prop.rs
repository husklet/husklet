//! Property application. One arm per property, each a single call.

use gtk::prelude::*;
use hl_gui::{Align, Length, Node, Prop, PropValue, Tag};

use crate::build;

/// Applies one property to an already constructed widget.
pub(crate) fn apply(widget: &gtk::Widget, node: &Node, prop: Prop, value: &PropValue) {
    match prop {
        Prop::Label | Prop::Value => text(widget, node.tag, value),
        Prop::Detail | Prop::Help => tooltip(widget, value),
        Prop::Placeholder => placeholder(widget, value),
        Prop::Icon => icon(widget, value),
        Prop::Tooltip => tooltip(widget, value),
        Prop::Uri => uri(widget, value),
        Prop::Enabled => widget.set_sensitive(value.as_flag().unwrap_or(true)),
        Prop::Visible => widget.set_visible(value.as_flag().unwrap_or(true)),
        Prop::Selected | Prop::Checked => checked(widget, value),
        Prop::Expanded => expanded(widget, value),
        Prop::Busy => busy(widget, value),
        Prop::Secret => secret(widget, value),
        Prop::Monospace => monospace(widget, value),
        Prop::Wrap => wrap(widget, value),
        Prop::Ellipsize => ellipsize(widget, value),
        Prop::Variant | Prop::Tone | Prop::Scale | Prop::Color => crate::style::mark(widget, prop, value),
        Prop::Gap => gap(widget, value),
        Prop::Pad => pad(widget, value),
        Prop::Grow => grow(widget, value),
        Prop::Width => width(widget, value),
        Prop::Height => height(widget, value),
        Prop::Align => align(widget, value),
        Prop::Justify => justify(widget, value),
        Prop::Orientation => orientation(widget, value),
        Prop::Position => position(widget, value),
        Prop::Minimum | Prop::Maximum | Prop::Step => range(widget, prop, value),
        Prop::Fraction => fraction(widget, value),
        Prop::Choices => choices(widget, value),
        Prop::Columns | Prop::Schema | Prop::Source | Prop::RowHeight => {
            crate::collection::configure(widget, prop, value);
        }
    }
}

/// Restores a property to its constructed default.
pub(crate) fn clear(widget: &gtk::Widget, node: &Node, prop: Prop) {
    apply(widget, node, prop, &PropValue::Nothing);
}

fn text(widget: &gtk::Widget, tag: Tag, value: &PropValue) {
    let content = value.as_text().unwrap_or_default();
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_text(content);
        return;
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        button.set_label(content);
        return;
    }
    // A check button is not a button subclass in GTK4, so it needs its own arm.
    if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
        check.set_label(Some(content));
        return;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.set_text(content);
        return;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>() {
        entry.set_text(content);
        return;
    }
    if let Some(view) = build::control::view(widget) {
        view.buffer().set_text(content);
        return;
    }
    if let Some(message) = build::surface::message(widget) {
        message.set_text(content);
        return;
    }
    numeric_text(widget, tag, value, content);
}

fn numeric_text(widget: &gtk::Widget, tag: Tag, value: &PropValue, content: &str) {
    if let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() {
        spin.set_value(value.as_number().unwrap_or_default());
        return;
    }
    if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        scale.set_value(value.as_number().unwrap_or_default());
        return;
    }
    if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
        expander.set_label(Some(content));
        return;
    }
    // A frame carries its title in its own header, above whatever it holds.
    if let Some(frame) = widget.downcast_ref::<gtk::Frame>() {
        frame.set_label(value.as_text());
        return;
    }
    if tag == Tag::Link {
        if let Some(link) = widget.downcast_ref::<gtk::LinkButton>() {
            link.set_label(content);
        }
        return;
    }
    caption(widget, tag, content);
}

/// A drawing area holds no text, so a chart's label is kept as its tooltip and
/// painted from there by the draw function.
fn caption(widget: &gtk::Widget, tag: Tag, content: &str) {
    if tag != Tag::Chart {
        return;
    }
    let Some(area) = widget.downcast_ref::<gtk::DrawingArea>() else {
        return;
    };
    area.set_tooltip_text(Some(content));
    area.queue_draw();
}

fn placeholder(widget: &gtk::Widget, value: &PropValue) {
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.set_placeholder_text(value.as_text());
    }
    if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>() {
        entry.set_placeholder_text(value.as_text());
    }
}

fn tooltip(widget: &gtk::Widget, value: &PropValue) {
    widget.set_tooltip_text(value.as_text());
}

fn icon(widget: &gtk::Widget, value: &PropValue) {
    let name = value.as_text();
    if let Some(image) = widget.downcast_ref::<gtk::Image>() {
        image.set_icon_name(name);
        return;
    }
    if let Some(emblem) = build::surface::emblem(widget) {
        emblem.set_icon_name(name);
        return;
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if let Some(name) = name {
            button.set_icon_name(name);
        }
    }
}

fn uri(widget: &gtk::Widget, value: &PropValue) {
    if let Some(link) = widget.downcast_ref::<gtk::LinkButton>() {
        link.set_uri(value.as_text().unwrap_or_default());
        return;
    }
    if let Some(picture) = widget.downcast_ref::<gtk::Picture>() {
        picture.set_file(value.as_text().map(source).as_ref());
    }
}

/// An absolute path and a `scheme://` reference both name a picture's file, and
/// only the second is a URI — reading a path as one silently loads nothing.
fn source(value: &str) -> gtk::gio::File {
    if value.contains("://") {
        return gtk::gio::File::for_uri(value);
    }
    gtk::gio::File::for_path(value)
}

fn checked(widget: &gtk::Widget, value: &PropValue) {
    let state = value.as_flag().unwrap_or(false);
    if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
        check.set_active(state);
        return;
    }
    if let Some(toggle) = widget.downcast_ref::<gtk::ToggleButton>() {
        toggle.set_active(state);
        return;
    }
    if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
        switch.set_active(state);
    }
}

fn expanded(widget: &gtk::Widget, value: &PropValue) {
    let state = value.as_flag().unwrap_or(false);
    if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
        expander.set_expanded(state);
        return;
    }
    if let Some(revealer) = widget.downcast_ref::<gtk::Revealer>() {
        revealer.set_reveal_child(state);
    }
}

fn busy(widget: &gtk::Widget, value: &PropValue) {
    let Some(spinner) = widget.downcast_ref::<gtk::Spinner>() else {
        return;
    };
    if value.as_flag().unwrap_or(true) {
        spinner.start();
    } else {
        spinner.stop();
    }
}

fn secret(widget: &gtk::Widget, value: &PropValue) {
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.set_visibility(!value.as_flag().unwrap_or(false));
    }
}

fn monospace(widget: &gtk::Widget, value: &PropValue) {
    if let Some(view) = build::control::view(widget) {
        view.set_monospace(value.as_flag().unwrap_or(true));
    }
}

fn wrap(widget: &gtk::Widget, value: &PropValue) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_wrap(value.as_flag().unwrap_or(false));
    }
}

fn ellipsize(widget: &gtk::Widget, value: &PropValue) {
    let Some(label) = widget.downcast_ref::<gtk::Label>() else {
        return;
    };
    let mode = if value.as_flag().unwrap_or(false) {
        gtk::pango::EllipsizeMode::End
    } else {
        gtk::pango::EllipsizeMode::None
    };
    label.set_ellipsize(mode);
}

fn gap(widget: &gtk::Widget, value: &PropValue) {
    let pixels = value.as_length().and_then(Length::pixels).unwrap_or(0);
    if let Some(container) = widget.downcast_ref::<gtk::Box>() {
        container.set_spacing(pixels.into());
        return;
    }
    if let Some(grid) = widget.downcast_ref::<gtk::Grid>() {
        grid.set_row_spacing(pixels.into());
        grid.set_column_spacing(pixels.into());
    }
}

fn pad(widget: &gtk::Widget, value: &PropValue) {
    let pixels = i32::from(value.as_length().and_then(Length::pixels).unwrap_or(0));
    widget.set_margin_top(pixels);
    widget.set_margin_bottom(pixels);
    widget.set_margin_start(pixels);
    widget.set_margin_end(pixels);
}

fn grow(widget: &gtk::Widget, value: &PropValue) {
    let expand = value.as_number().unwrap_or_default() > 0.0;
    widget.set_hexpand(expand);
    widget.set_vexpand(expand);
}

fn width(widget: &gtk::Widget, value: &PropValue) {
    match value.as_length() {
        Some(Length::Fill) => widget.set_hexpand(true),
        Some(Length::Chars(count)) => characters(widget, count),
        Some(Length::Step(step)) => {
            widget.set_size_request(i32::from(Length::Step(step).pixels().unwrap_or(0)), -1);
        }
        _ => widget.set_hexpand(false),
    }
}

fn characters(widget: &gtk::Widget, count: u16) {
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.set_width_chars(count.into());
        return;
    }
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_width_chars(count.into());
    }
}

fn height(widget: &gtk::Widget, value: &PropValue) {
    match value.as_length() {
        Some(Length::Fill) => widget.set_vexpand(true),
        Some(Length::Step(step)) => {
            widget.set_size_request(-1, i32::from(Length::Step(step).pixels().unwrap_or(0)));
        }
        _ => widget.set_vexpand(false),
    }
}

fn align(widget: &gtk::Widget, value: &PropValue) {
    let PropValue::Align(align) = value else {
        return;
    };
    widget.set_halign(placement(*align));
}

fn justify(widget: &gtk::Widget, value: &PropValue) {
    let PropValue::Align(align) = value else {
        return;
    };
    widget.set_valign(placement(*align));
}

fn placement(align: Align) -> gtk::Align {
    match align {
        Align::Start => gtk::Align::Start,
        Align::Center => gtk::Align::Center,
        Align::End => gtk::Align::End,
        Align::Stretch => gtk::Align::Fill,
    }
}

fn orientation(widget: &gtk::Widget, value: &PropValue) {
    let PropValue::Orientation(orientation) = value else {
        return;
    };
    let axis = match orientation {
        hl_gui::Orientation::Horizontal => gtk::Orientation::Horizontal,
        hl_gui::Orientation::Vertical => gtk::Orientation::Vertical,
    };
    if let Some(container) = widget.downcast_ref::<gtk::Box>() {
        container.set_orientation(axis);
        return;
    }
    if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
        paned.set_orientation(axis);
        return;
    }
    if let Some(separator) = widget.downcast_ref::<gtk::Separator>() {
        separator.set_orientation(axis);
    }
}

fn position(widget: &gtk::Widget, value: &PropValue) {
    if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
        paned.set_position(value.as_number().unwrap_or_default() as i32);
    }
}

fn range(widget: &gtk::Widget, prop: Prop, value: &PropValue) {
    let Some(number) = value.as_number() else {
        return;
    };
    if let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() {
        bound(&spin.adjustment(), prop, number);
        return;
    }
    if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        bound(&scale.adjustment(), prop, number);
    }
}

fn bound(adjustment: &gtk::Adjustment, prop: Prop, number: f64) {
    match prop {
        Prop::Minimum => adjustment.set_lower(number),
        Prop::Maximum => adjustment.set_upper(number),
        _ => adjustment.set_step_increment(number),
    }
}

fn fraction(widget: &gtk::Widget, value: &PropValue) {
    if let Some(progress) = widget.downcast_ref::<gtk::ProgressBar>() {
        progress.set_fraction(value.as_number().unwrap_or_default().clamp(0.0, 1.0));
    }
}

fn choices(widget: &gtk::Widget, value: &PropValue) {
    let PropValue::Choices(choices) = value else {
        return;
    };
    let labels: Vec<&str> = choices.iter().map(|choice| choice.label.as_str()).collect();
    if let Some(drop) = widget.downcast_ref::<gtk::DropDown>() {
        drop.set_model(Some(&gtk::StringList::new(&labels)));
        return;
    }
    radios(widget, &labels);
}

fn radios(widget: &gtk::Widget, labels: &[&str]) {
    let Some(container) = widget.downcast_ref::<gtk::Box>() else {
        return;
    };
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let mut group: Option<gtk::CheckButton> = None;
    for label in labels {
        let button = gtk::CheckButton::with_label(label);
        if let Some(first) = group.as_ref() {
            button.set_group(Some(first));
        } else {
            button.set_active(true);
            group = Some(button.clone());
        }
        container.append(&button);
    }
}
