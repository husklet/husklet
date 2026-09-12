//! Property application. One arm per property, each a single call.

use gtk::prelude::*;
use hl_gui::{Length, Node, Orientation, Prop, PropValue, Tag};

use crate::build;
use crate::component::{choice, feedback, field};
use crate::text;

/// Applies one property to an already constructed widget.
pub(crate) fn apply(widget: &gtk::Widget, node: &Node, prop: Prop, value: &PropValue, reports: &crate::event::Reports) {
    match prop {
        Prop::Label => text::caption(widget, node.tag, value),
        Prop::Value if node.tag == Tag::Select => select_value(widget, node),
        Prop::Value => text::body(widget, node.tag, value),
        Prop::Detail => text::detail(widget, value),
        Prop::Help | Prop::Tooltip => tooltip(widget, value),
        Prop::Placeholder => text::placeholder(widget, value),
        Prop::Icon => {
            text::icon(widget, value);
            if node.tag == Tag::Button && node.prop(Prop::Busy).and_then(PropValue::as_flag).unwrap_or(false) {
                if let Some(emblem) = crate::component::slot::emblem(widget) {
                    emblem.set_visible(false);
                }
            }
            if value.as_text().is_none() {
                feedback::tone(widget, node, node.prop(Prop::Tone).unwrap_or(&PropValue::Nothing));
            }
        }
        Prop::Uri => text::uri(widget, value),
        Prop::Enabled => enabled(widget, node, value),
        Prop::Visible => widget.set_visible(value.as_flag().unwrap_or(true)),
        Prop::Selected | Prop::Checked => checked(widget, value),
        Prop::Indeterminate => indeterminate(widget, value),
        Prop::Expanded => expanded(widget, value),
        Prop::Busy => busy(widget, node, value),
        Prop::Secret => secret(widget, value),
        // Product-authored semantic metadata. It must survive in the retained
        // tree, but has no visual state for a toolkit adapter to manufacture.
        Prop::Destructive => {}
        Prop::Monospace => monospace(widget, value),
        Prop::Wrap => wrap(widget, node, value),
        Prop::Ellipsize => ellipsize(widget, value),
        Prop::Variant | Prop::Tone | Prop::Scale | Prop::Size | Prop::Color => {
            crate::style::mark(widget, prop, value);
            if prop == Prop::Tone {
                feedback::tone(widget, node, value);
            }
        }
        Prop::Gap => gap(widget, value),
        Prop::Pad => pad(widget, value),
        Prop::Grow => grow(widget, value),
        Prop::Width => {
            width(widget, value);
            crate::component::table::authored_width(widget, value);
        }
        Prop::Height => height(widget, value),
        Prop::Align | Prop::Justify => build::layout::alignment(widget, prop, value),
        Prop::Span | Prop::RowSpan => build::layout::span(widget, prop, value),
        Prop::Orientation => orientation(widget, value),
        Prop::Position => position(widget, value),
        Prop::Breakpoint => build::responsive::set(widget, value),
        Prop::Minimum | Prop::Maximum | Prop::Step => range(widget, prop, value),
        Prop::Fraction => fraction(widget, value),
        Prop::Choices => choices(widget, node, value, reports),
        Prop::Columns => columns(widget, value),
        Prop::Schema | Prop::Source | Prop::RowHeight => {
            crate::collection::configure(widget, node, prop, value, reports);
        }
    }
}

/// Restores a property to its constructed default.
pub(crate) fn clear(widget: &gtk::Widget, node: &Node, prop: Prop, reports: &crate::event::Reports) {
    apply(widget, node, prop, &PropValue::Nothing, reports);
    if prop == Prop::Size && matches!(node.tag, Tag::Button | Tag::IconButton) {
        widget.add_css_class("size-medium");
    }
}

fn tooltip(widget: &gtk::Widget, value: &PropValue) {
    widget.set_tooltip_text(value.as_text());
}

fn enabled(widget: &gtk::Widget, node: &Node, value: &PropValue) {
    let busy = node.tag == Tag::Button && node.prop(Prop::Busy).and_then(PropValue::as_flag).unwrap_or(false);
    widget.set_sensitive(value.as_flag().unwrap_or(true) && !busy);
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

fn indeterminate(widget: &gtk::Widget, value: &PropValue) {
    if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
        check.set_inconsistent(value.as_flag().unwrap_or(false));
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

fn busy(widget: &gtk::Widget, node: &Node, value: &PropValue) {
    let state = value.as_flag().unwrap_or(node.tag == Tag::Spinner);
    let spinner = widget
        .downcast_ref::<gtk::Spinner>()
        .cloned()
        .or_else(|| crate::component::slot::activity(widget));
    let Some(spinner) = spinner else { return };
    if state {
        spinner.start();
    } else {
        spinner.stop();
    }
    spinner.set_visible(state);

    if node.tag == Tag::Button {
        widget.update_state(&[gtk::accessible::State::Busy(state)]);
        if let Some(emblem) = crate::component::slot::emblem(widget) {
            let has_icon = node
                .prop(Prop::Icon)
                .and_then(PropValue::as_text)
                .is_some_and(|icon| !icon.is_empty());
            emblem.set_visible(!state && has_icon);
        }
        let enabled = node.prop(Prop::Enabled).and_then(PropValue::as_flag).unwrap_or(true);
        widget.set_sensitive(!state && enabled);
        if state {
            widget.add_css_class("hl-busy");
        } else {
            widget.remove_css_class("hl-busy");
        }
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
    // A gallery lays its pictures out in lines of its own, so its column count
    // is how many it puts on one line before wrapping.
    if let Some(gallery) = widget.downcast_ref::<gtk::FlowBox>() {
        gallery.set_max_children_per_line(u32::from(value.as_count().unwrap_or(1)));
        return;
    }
    // Legacy non-virtual tables do not use the report channel.
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
    if crate::component::card::gap(widget, pixels) {
        return;
    }
    // A revealed surface holds its children in the column it reveals, so the
    // space between them is that column's, not the revealer's.
    let revealed = widget
        .downcast_ref::<gtk::Revealer>()
        .and_then(gtk::Revealer::child)
        .filter(gtk::prelude::ObjectExt::is::<gtk::Box>);
    let widget = revealed.as_ref().unwrap_or(widget);
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
        return;
    }
    // A gallery wraps its own children, and the space between them is the same
    // property a row means by it.
    if let Some(gallery) = widget.downcast_ref::<gtk::FlowBox>() {
        let spacing = u32::try_from(pixels).unwrap_or(0);
        gallery.set_row_spacing(spacing);
        gallery.set_column_spacing(spacing);
    }
}

/// Space inside a widget's own edges, per side.
fn pad(widget: &gtk::Widget, value: &PropValue) {
    let edges = value.as_edges().unwrap_or_default();
    for class in widget.css_classes() {
        if class.starts_with("pad-") {
            widget.remove_css_class(&class);
        }
    }
    for (side, length) in [
        ("top", edges.top),
        ("end", edges.end),
        ("bottom", edges.bottom),
        ("start", edges.start),
    ] {
        if let Length::Step(step) = length {
            widget.add_css_class(&format!("pad-{side}-{}", Length::clamp(step)));
        }
    }
}

fn grow(widget: &gtk::Widget, value: &PropValue) {
    let expand = value.as_number().unwrap_or_default() > 0.0;
    widget.set_hexpand(expand);
    widget.set_vexpand(expand);
    if expand {
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
    }
}

fn width(widget: &gtk::Widget, value: &PropValue) {
    if let PropValue::Bounds(bounds) = value {
        span_across(widget, gtk::Orientation::Horizontal, *bounds);
        return;
    }
    match value.as_length() {
        Some(Length::Fill) => {
            widget.set_hexpand(true);
            widget.set_halign(gtk::Align::Fill);
        }
        Some(Length::Chars(count)) => characters(widget, count),
        Some(Length::Step(step)) => {
            widget.set_size_request(i32::from(Length::Step(step).pixels().unwrap_or(0)), -1);
        }
        _ => widget.set_hexpand(false),
    }
}

fn characters(widget: &gtk::Widget, count: u16) {
    if let Some(choice) = widget.downcast_ref::<crate::component::choice::Choice>() {
        choice.set_width_chars(count.into());
        return;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.set_width_chars(count.into());
        return;
    }
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_width_chars(count.into());
        return;
    }
    // Containers and scrolling views do not expose `width-chars`, but Chars is
    // a universal layout value. Resolve it through the widget's actual font so
    // a 26-character navigation pane does not silently collapse to a few pixels.
    let units = widget.pango_context().metrics(None, None).approximate_char_width();
    let pixels = units
        .saturating_mul(i32::from(count))
        .saturating_add(gtk::pango::SCALE - 1)
        / gtk::pango::SCALE;
    widget.set_size_request(pixels.max(1), widget.height_request());
}

/// A floor and a ceiling on one axis.
///
/// The floor is a size request, which every widget honours. The ceiling is only
/// honoured where GTK4 can express one — scrolled content and character-counted
/// text — because a plain GTK widget has no maximum size, and inventing one by
/// freezing the widget at its floor would silently render a size the producer
/// never described. A vertical ceiling also opts a scroller into natural-height
/// propagation: sparse content stays at the floor while dense content grows only
/// to the declared maximum.
fn span_across(widget: &gtk::Widget, axis: gtk::Orientation, bounds: hl_gui::Bounds) {
    let horizontal = axis == gtk::Orientation::Horizontal;
    if let Some(pixels) = bounds.minimum.and_then(|length| dimension_pixels(length, horizontal)) {
        let request = i32::from(pixels);
        if let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() {
            if horizontal {
                window.set_min_content_width(request);
            } else {
                window.set_min_content_height(request);
            }
        }
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

fn dimension_pixels(length: Length, horizontal: bool) -> Option<u16> {
    match length {
        // Step classes clamp because only a bounded CSS vocabulary exists.
        // Explicit vertical dimensions are values, not class names: step 80
        // must remain 320px rather than collapsing to the largest padding.
        Length::Step(step) if !horizontal => Some(u16::from(step) * Length::STEP_PIXELS),
        _ => length.pixels(),
    }
}

fn ceiling_of(widget: &gtk::Widget, horizontal: bool, ceiling: Length) {
    if horizontal && ceiling == Length::Fill {
        widget.set_hexpand(true);
        widget.set_halign(gtk::Align::Fill);
        return;
    }
    if let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() {
        let pixels = i32::from(dimension_pixels(ceiling, horizontal).unwrap_or(0));
        if horizontal {
            window.set_max_content_width(pixels);
        } else {
            window.set_max_content_height(pixels);
            window.set_propagate_natural_height(true);
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
        widget.set_vexpand(false);
        widget.set_size_request(widget.width_request(), -1);
        span_across(widget, gtk::Orientation::Vertical, *bounds);
        return;
    }
    match value.as_length() {
        Some(Length::Fill) => {
            widget.set_size_request(widget.width_request(), -1);
            widget.set_vexpand(true);
        }
        Some(Length::Step(step)) => {
            widget.set_vexpand(false);
            widget.set_size_request(widget.width_request(), i32::from(u16::from(step) * Length::STEP_PIXELS));
        }
        _ => {
            widget.set_vexpand(false);
            widget.set_size_request(widget.width_request(), -1);
        }
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
    if let Some(paned) = build::responsive::paned(widget) {
        paned.set_orientation(axis);
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
    let position = value.as_number().unwrap_or_default() as i32;
    if build::responsive::set_position(widget, position) {
        return;
    }
    if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
        paned.set_position(position);
    }
}

fn range(widget: &gtk::Widget, prop: Prop, value: &PropValue) {
    let Some(number) = value.as_number() else {
        return;
    };
    if let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() {
        bound(&spin.adjustment(), prop, number);
        if prop == Prop::Step {
            spin.set_digits(decimal_digits(number));
        }
        return;
    }
    if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        bound(&scale.adjustment(), prop, number);
    }
}

fn decimal_digits(step: f64) -> u32 {
    if !step.is_finite() || step <= 0.0 {
        return 0;
    }
    (0..=6)
        .find(|digits| {
            let scaled = step * 10_f64.powi(*digits as i32);
            (scaled - scaled.round()).abs() <= 1e-9
        })
        .unwrap_or(6)
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

fn choices(widget: &gtk::Widget, node: &Node, value: &PropValue, reports: &crate::event::Reports) {
    let PropValue::Choices(choices) = value else {
        reports.set_choices(node.id, Vec::new());
        choice::set_options(widget, &[]);
        return;
    };
    reports.set_choices(node.id, choices.iter().map(|choice| choice.value.clone()).collect());
    let labels: Vec<&str> = choices.iter().map(|choice| choice.label.as_str()).collect();
    if choice::set_options(widget, &labels) {
        select_value(widget, node);
        return;
    }
    if let Some(drop) = widget.downcast_ref::<gtk::DropDown>() {
        drop.set_model(Some(&gtk::StringList::new(&labels)));
    }
}

/// Selects the option whose stable producer value matches `value`.
///
/// GTK's model displays labels while Husklet's retained value is deliberately
/// independent of those labels, so changing copy cannot change application state.
fn select_value(widget: &gtk::Widget, node: &Node) {
    if !widget.is::<choice::Choice>() {
        return;
    }
    let wanted = node.prop(Prop::Value).and_then(PropValue::as_text);
    let Some(PropValue::Choices(choices)) = node.prop(Prop::Choices) else {
        return;
    };
    let selected = wanted
        .and_then(|wanted| choices.iter().position(|choice| choice.value == wanted))
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or(gtk::INVALID_LIST_POSITION);
    choice::set_selected(widget, (selected != gtk::INVALID_LIST_POSITION).then_some(selected));
}

#[cfg(test)]
mod tests {
    use super::{decimal_digits, grow, height, width};
    use gtk::prelude::*;
    use hl_gui::{Length, PropValue};

    #[test]
    fn number_entry_precision_follows_its_declared_step() {
        assert_eq!(decimal_digits(1.0), 0);
        assert_eq!(decimal_digits(0.5), 1);
        assert_eq!(decimal_digits(0.25), 2);
        assert_eq!(decimal_digits(0.001), 3);
        assert_eq!(decimal_digits(0.0), 0);
    }

    #[test]
    fn select_is_compact_until_its_author_explicitly_enables_growth() {
        let _ = crate::test_support::on_the_toolkit_thread(|| {
            let control: gtk::Widget = crate::component::choice::widget().upcast();

            width(&control, &PropValue::Length(Length::Chars(20)));
            assert!(!control.hexpands(), "a character width must remain compact");

            grow(&control, &PropValue::Number(1.0));
            assert!(control.hexpands(), "Grow must retain explicit fill authority");
        });
    }

    #[test]
    fn authored_card_width_does_not_gain_implicit_fill_authority() {
        let _ = crate::test_support::on_the_toolkit_thread(|| {
            let card: gtk::Widget = crate::component::card::widget(hl_gui::Tag::Card);
            assert!(
                !card.hexpands(),
                "cards are compact until their author opts into growth"
            );
            assert_eq!(card.halign(), gtk::Align::Start);
            width(&card, &PropValue::Length(Length::Chars(38)));
            assert!(!card.hexpands(), "authored card width governs the outer native frame");
            assert!(card.width_request() > 0);
            grow(&card, &PropValue::Number(1.0));
            assert!(card.hexpands(), "explicit grow grants fill authority");
            assert_eq!(card.halign(), gtk::Align::Fill);
        });
    }

    #[test]
    fn a_fill_ceiling_expands_from_an_authored_width_floor_without_vertical_growth() {
        let _ = crate::test_support::on_the_toolkit_thread(|| {
            let card: gtk::Widget = crate::component::card::widget(hl_gui::Tag::Card);
            width(
                &card,
                &PropValue::Bounds(hl_gui::Bounds {
                    minimum: Some(Length::Chars(38)),
                    maximum: Some(Length::Fill),
                }),
            );

            assert!(card.width_request() > 0, "the character floor remains measurable");
            assert!(card.hexpands(), "Fill grants available horizontal width");
            assert_eq!(card.halign(), gtk::Align::Fill);
            assert!(!card.vexpands(), "a width bound must not grant vertical growth");
        });
    }

    #[test]
    fn data_table_height_and_growth_follow_latest_expansion_property() {
        let _ = crate::test_support::on_the_toolkit_thread(|| {
            let table: gtk::Widget = gtk::ScrolledWindow::new().upcast();
            table.set_size_request(123, -1);

            assert!(!table.vexpands(), "DataTable begins without vertical expansion");
            height(&table, &PropValue::Length(Length::Step(80)));
            assert_eq!((table.width_request(), table.height_request()), (123, 320));
            assert!(!table.vexpands(), "a fixed height is not an expansion request");

            height(&table, &PropValue::Length(Length::Fill));
            assert_eq!(table.height_request(), -1, "Fill clears a stale fixed request");
            assert!(table.vexpands(), "Fill explicitly opts into expansion");
            height(&table, &PropValue::Length(Length::Step(80)));
            assert_eq!(table.height_request(), 320);
            assert!(!table.vexpands(), "Fill followed by Step becomes fixed again");

            height(
                &table,
                &PropValue::Bounds(hl_gui::Bounds {
                    minimum: Some(Length::Step(20)),
                    maximum: Some(Length::Step(80)),
                }),
            );
            let window = table
                .downcast_ref::<gtk::ScrolledWindow>()
                .expect("DataTable is a scrolling viewport");
            assert_eq!(table.height_request(), 80);
            assert_eq!(window.min_content_height(), 80);
            assert_eq!(window.max_content_height(), 320);
            assert!(
                window.propagates_natural_height(),
                "a bounded viewport follows sparse content until its ceiling"
            );

            grow(&table, &PropValue::Number(1.0));
            assert!(table.vexpands(), "Grow explicitly opts into expansion");
            height(&table, &PropValue::Length(Length::Step(80)));
            assert!(!table.vexpands(), "Step following Grow clears vertical expansion");
            grow(&table, &PropValue::Number(1.0));
            assert!(table.vexpands(), "a later Grow remains an explicit opt-in");
            grow(&table, &PropValue::Number(0.0));
            assert!(!table.vexpands(), "clearing Grow clears expansion");
        });
    }
}
