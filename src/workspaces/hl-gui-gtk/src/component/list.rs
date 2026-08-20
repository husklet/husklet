//! Lists and the parts one row is composed from.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Row-oriented components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::List => list().upcast(),
        Tag::ListRow => row().upcast(),
        Tag::ListItemText => lines().upcast(),
        Tag::ListItemButton => button().upcast(),
        // ListItemAction and ListItemSecondaryAction are the last list tags
        // routed here: both are a group of controls at the end of a row, and
        // which of the two trails the other is decided by placement, not by
        // being a different widget.
        _ => trailing().upcast(),
    }
}

fn list() -> gtk::ScrolledWindow {
    let view = gtk::ListBox::new();
    view.set_selection_mode(gtk::SelectionMode::Single);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&view));
    window.set_vexpand(true);
    window
}

fn row() -> gtk::Box {
    let widget = axis::row(8);
    widget.set_hexpand(true);
    widget
}

/// The primary and secondary text of a row, stacked.
fn lines() -> gtk::Box {
    let widget = axis::column(0);
    widget.set_hexpand(true);
    widget.set_valign(gtk::Align::Center);
    widget.append(&slot::caption_label());
    widget.append(&slot::detail_label());
    widget
}

fn button() -> gtk::Button {
    let widget = axis::item();
    widget.set_hexpand(true);
    widget
}

fn trailing() -> gtk::Box {
    let widget = axis::row(4);
    widget.set_halign(gtk::Align::End);
    widget.set_valign(gtk::Align::Center);
    widget
}

/// The list box behind a list component, when the widget is one.
pub(crate) fn rows(widget: &gtk::Widget) -> Option<gtk::ListBox> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::ListBox>().ok())
}

/// The parts of a row, in the order a row is read in.
///
/// One order, consulted both for the part being placed and for the parts
/// already there, is what lets a producer describe them in any order at all —
/// and what keeps the trailing action last even when the action beside it was
/// described afterwards.
const ORDER: [Tag; 5] = [
    Tag::ListItemIcon,
    Tag::ListItemAvatar,
    Tag::ListItemText,
    Tag::ListItemAction,
    Tag::ListItemSecondaryAction,
];

fn rank(tag: Tag) -> Option<usize> {
    ORDER.iter().position(|held| *held == tag)
}

/// Places a row's parts where a row reads from: marks lead, text takes the
/// space between, controls trail, and the trailing action comes after them.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    if !super::belongs(parent, Tag::ListRow) {
        return false;
    }
    let (Some(container), Some(place)) = (parent.downcast_ref::<gtk::Box>(), rank(tag)) else {
        return false;
    };
    seat(container, child, place);
    true
}

/// Inserts a part ahead of the first part that reads after it.
fn seat(container: &gtk::Box, child: &gtk::Widget, place: usize) {
    let later = slot::offspring(container.upcast_ref())
        .into_iter()
        .find(|held| seated(held).is_some_and(|found| found > place));
    match later {
        Some(next) => container.insert_child_after(child, next.prev_sibling().as_ref()),
        None => container.append(child),
    }
}

/// Which part of a row a widget already in it is.
fn seated(widget: &gtk::Widget) -> Option<usize> {
    ORDER.iter().position(|tag| super::belongs(widget, *tag))
}
