//! Property application. One arm per property, each a single call.

use gtk::prelude::*;
use hl_gui::{Length, Node, Orientation, Prop, PropValue, Tag};

use crate::build;
use crate::component::field;
use crate::text;

/// Applies one property to an already constructed widget.
pub(crate) fn apply(widget: &gtk::Widget, node: &Node, prop: Prop, value: &PropValue) {
    match prop {
        Prop::Label => text::caption(widget, node.tag, value),
        Prop::Value => text::body(widget, node.tag, value),
        Prop::Detail => text::detail(widget, value),
        Prop::Help | Prop::Tooltip => tooltip(widget, value),
        Prop::Placeholder => text::placeholder(widget, value),
        Prop::Icon => text::icon(widget, value),
        Prop::Uri => text::uri(widget, value),
        Prop::Enabled => widget.set_sensitive(value.as_flag().unwrap_or(true)),
        Prop::Visible => widget.set_visible(value.as_flag().unwrap_or(true)),
        Prop::Selected | Prop::Checked => checked(widget, value),
        Prop::Expanded => expanded(widget, value),
        Prop::Busy => busy(widget, value),
        Prop::Secret => secret(widget, value),
        Prop::Monospace => monospace(widget, value),
        Prop::Wrap => wrap(widget, node, value),
        Prop::Ellipsize => ellipsize(widget, value),
        Prop::Variant | Prop::Tone | Prop::Scale | Prop::Color => crate::style::mark(widget, prop, value),
        Prop::Gap => gap(widget, value),
        Prop::Pad => pad(widget, value),
        Prop::Grow => grow(widget, value),
        Prop::Width => width(widget, value),
        Prop::Height => height(widget, value),
        Prop::Align | Prop::Justify => build::layout::alignment(widget, prop, value),
        Prop::Span | Prop::RowSpan => build::layout::span(widget, prop, value),
        Prop::Orientation => orientation(widget, value),
        Prop::Position => position(widget, value),
        Prop::Minimum | Prop::Maximum | Prop::Step => range(widget, prop, value),
        Prop::Fraction => fraction(widget, value),
        Prop::Choices => choices(widget, value),
        Prop::Columns => columns(widget, value),
        Prop::Schema | Prop::Source | Prop::RowHeight => {
            crate::collection::configure(widget, prop, value);
        }
    }
}

/// Restores a property to its constructed default.
pub(crate) fn clear(widget: &gtk::Widget, node: &Node, prop: Prop) {
    apply(widget, node, prop, &PropValue::Nothing);
}

fn tooltip(widget: &gtk::Widget, value: &PropValue) {
    widget.set_tooltip_text(value.as_text());
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
    // A password entry is secret by construction; what the property decides
    // there is whether a person may peek at what they typed.
    if let Some(entry) = widget.downcast_ref::<gtk::PasswordEntry>() {
        entry.set_show_peek_icon(!value.as_flag().unwrap_or(false));
    }
}

fn monospace(widget: &gtk::Widget, value: &PropValue) {
    if let Some(view) = field::view(widget) {
        view.set_monospace(value.as_flag().unwrap_or(true));
    }
}

/// Words onto further lines for a text node; whole children onto further lines
/// for a row or a column.
fn wrap(widget: &gtk::Widget, node: &Node, value: &PropValue) {
    let wrapping = value.as_flag().unwrap_or(false);
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_wrap(wrapping);
        return;
    }
    build::layout::wrap(widget, axis(node), wrapping);
}

/// The axis a container advances along: whichever the producer described, or
/// the one its tag stands for.
fn axis(node: &Node) -> Orientation {
    match node.prop(Prop::Orientation) {
        Some(PropValue::Orientation(orientation)) => *orientation,
        _ if node.tag == Tag::Column => Orientation::Vertical,
        _ => Orientation::Horizontal,
    }
}

/// How many columns a grid lays its children out in. Every other widget reading
/// this property is a collection declaring how many columns it has.
fn columns(widget: &gtk::Widget, value: &PropValue) {
    if widget.downcast_ref::<gtk::Grid>().is_some() {
        build::layout::columns(widget, value);
        return;
    }
    crate::collection::configure(widget, Prop::Columns, value);
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
    let pixels = i32::from(value.as_length().and_then(Length::pixels).unwrap_or(0));
    // A wrapping container runs its own layout, and asking the box for its
    // spacing would configure the layout it no longer uses.
    if build::layout::gap(widget, pixels) {
        return;
    }
    if let Some(container) = widget.downcast_ref::<gtk::Box>() {
        container.set_spacing(pixels);
        return;
    }
    if let Some(grid) = widget.downcast_ref::<gtk::Grid>() {
        let spacing = u32::try_from(pixels).unwrap_or(0);
        grid.set_row_spacing(spacing);
        grid.set_column_spacing(spacing);
    }
}

/// Space inside a widget's own edges, per side.
fn pad(widget: &gtk::Widget, value: &PropValue) {
    let edges = value.as_edges().unwrap_or_default();
    widget.set_margin_top(margin(edges.top));
    widget.set_margin_bottom(margin(edges.bottom));
    widget.set_margin_start(margin(edges.start));
    widget.set_margin_end(margin(edges.end));
}

/// A side's space in pixels. A length with no pixel size — `Fill`, `Content`,
/// a character count — describes no margin, which is no margin at all.
fn margin(length: Length) -> i32 {
    i32::from(length.pixels().unwrap_or(0))
}

fn grow(widget: &gtk::Widget, value: &PropValue) {
    let expand = value.as_number().unwrap_or_default() > 0.0;
    widget.set_hexpand(expand);
    widget.set_vexpand(expand);
}

fn width(widget: &gtk::Widget, value: &PropValue) {
    if let PropValue::Bounds(bounds) = value {
        span_across(widget, gtk::Orientation::Horizontal, *bounds);
        return;
    }
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

/// A floor and a ceiling on one axis.
///
/// The floor is a size request, which every widget honours. The ceiling is only
/// honoured where GTK4 can express one — scrolled content and character-counted
/// text — because a plain GTK widget has no maximum size, and inventing one by
/// freezing the widget at its floor would silently render a size the producer
/// never described.
fn span_across(widget: &gtk::Widget, axis: gtk::Orientation, bounds: hl_gui::Bounds) {
    let horizontal = axis == gtk::Orientation::Horizontal;
    if let Some(pixels) = bounds.minimum.and_then(Length::pixels) {
        let request = i32::from(pixels);
        // One size request carries both axes, so a floor on one of them must
        // carry the other axis forward or describing a height would erase a
        // width the producer asked for a patch earlier.
        let (width, height) = if horizontal {
            (request, widget.height_request())
        } else {
            (widget.width_request(), request)
        };
        widget.set_size_request(width, height);
    }
    if let Some(Length::Chars(count)) = bounds.minimum {
        characters(widget, count);
    }
    let Some(ceiling) = bounds.maximum else {
        return;
    };
    ceiling_of(widget, horizontal, ceiling);
}

fn ceiling_of(widget: &gtk::Widget, horizontal: bool, ceiling: Length) {
    if let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() {
        let pixels = i32::from(ceiling.pixels().unwrap_or(0));
        if horizontal {
            window.set_max_content_width(pixels);
        } else {
            window.set_max_content_height(pixels);
        }
        return;
    }
    let (Some(label), Length::Chars(count)) = (widget.downcast_ref::<gtk::Label>(), ceiling) else {
        return;
    };
    if horizontal {
        label.set_max_width_chars(count.into());
    }
}

fn height(widget: &gtk::Widget, value: &PropValue) {
    if let PropValue::Bounds(bounds) = value {
        span_across(widget, gtk::Orientation::Vertical, *bounds);
        return;
    }
    match value.as_length() {
        Some(Length::Fill) => widget.set_vexpand(true),
        Some(Length::Step(step)) => {
            widget.set_size_request(-1, i32::from(Length::Step(step).pixels().unwrap_or(0)));
        }
        _ => widget.set_vexpand(false),
    }
}

fn orientation(widget: &gtk::Widget, value: &PropValue) {
    let PropValue::Orientation(orientation) = value else {
        return;
    };
    // A wrapping container keeps its axis in its own layout; reaching past that
    // to the box would configure the layout it stopped using.
    if build::layout::direct(widget, *orientation) {
        return;
    }
    let axis = match orientation {
        Orientation::Horizontal => gtk::Orientation::Horizontal,
        Orientation::Vertical => gtk::Orientation::Vertical,
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
    let filled = value.as_number().unwrap_or_default().clamp(0.0, 1.0);
    if let Some(progress) = widget.downcast_ref::<gtk::ProgressBar>() {
        progress.set_fraction(filled);
        return;
    }
    // A level bar measures against a maximum of one by construction, so the
    // same fraction reads as the same proportion of the bar.
    if let Some(level) = widget.downcast_ref::<gtk::LevelBar>() {
        level.set_value(filled);
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
