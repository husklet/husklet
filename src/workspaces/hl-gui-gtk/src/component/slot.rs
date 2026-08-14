//! The named parts a composite widget carries its own properties in.
//!
//! Most components in the library are more than one GTK widget: a card header
//! is an icon, a title and a subtitle, and a statistic is a figure above its
//! caption. Their `Label`, `Detail` and `Icon` properties have to reach a
//! specific widget inside that composition, and property application only ever
//! sees the outer widget.
//!
//! A builder therefore stamps a style class on the slot it wants a property to
//! land in, and application looks the slot up by that class. The classes are
//! only ever set here, so a described child can never be mistaken for a slot,
//! and the same lookup works for every composite without property application
//! learning the shape of any of them.

use gtk::prelude::*;

/// The label a composite shows its own `Label` in.
const CAPTION: &str = "hl-caption";
/// The label a composite shows its own `Detail` in.
const DETAIL: &str = "hl-detail";
/// The image a composite shows its own `Icon` in.
const EMBLEM: &str = "hl-emblem";
/// The editable a composite holds its own `Value` in.
const FIELD: &str = "hl-field";

/// A label built to receive its composite's `Label`.
pub(crate) fn caption_label() -> gtk::Label {
    text(CAPTION)
}

/// A label built to receive its composite's `Detail`.
pub(crate) fn detail_label() -> gtk::Label {
    let label = text(DETAIL);
    label.add_css_class("dim-label");
    label
}

/// An image built to receive its composite's `Icon`.
pub(crate) fn emblem_image() -> gtk::Image {
    let image = gtk::Image::new();
    image.add_css_class(EMBLEM);
    image
}

/// Marks an editable as the slot its composite's `Value` belongs in.
pub(crate) fn field(entry: &impl IsA<gtk::Widget>) {
    entry.as_ref().add_css_class(FIELD);
}

fn text(class: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_halign(gtk::Align::Start);
    // Aligning the widget is not enough: a label that fills its slot still
    // centres its own text, so a caption would read from the middle.
    label.set_xalign(0.0);
    label.add_css_class(class);
    label
}

/// The caption slot of a composite, when it has one.
pub(crate) fn caption(widget: &gtk::Widget) -> Option<gtk::Label> {
    descend(widget, CAPTION)?.downcast().ok()
}

/// The detail slot of a composite, when it has one.
pub(crate) fn detail(widget: &gtk::Widget) -> Option<gtk::Label> {
    descend(widget, DETAIL)?.downcast().ok()
}

/// The icon slot of a composite, when it has one.
pub(crate) fn emblem(widget: &gtk::Widget) -> Option<gtk::Image> {
    descend(widget, EMBLEM)?.downcast().ok()
}

/// The editable slot of a composite, when it has one.
pub(crate) fn editable(widget: &gtk::Widget) -> Option<gtk::Widget> {
    descend(widget, FIELD)
}

/// The nearest descendant carrying a slot class.
///
/// Nearest rather than first in document order: a composite builds its own
/// slots directly beneath itself, so a slot belonging to a described child is
/// always further away and never wins.
fn descend(widget: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    let mut generation = offspring(widget);
    while !generation.is_empty() {
        if let Some(found) = generation.iter().find(|child| child.has_css_class(class)) {
            return Some(found.clone());
        }
        generation = generation.iter().flat_map(offspring).collect();
    }
    None
}

/// Every direct child of a widget, in order.
pub(crate) fn offspring(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut children = Vec::new();
    let mut cursor = widget.first_child();
    while let Some(child) = cursor {
        cursor = child.next_sibling();
        children.push(child);
    }
    children
}
