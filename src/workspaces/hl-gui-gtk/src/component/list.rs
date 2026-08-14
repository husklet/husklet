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
        // ListItemAction is the last list tag routed here.
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

/// Places a row's parts where a row reads from: marks lead, text takes the
/// space between, and controls trail.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    if !super::belongs(parent, Tag::ListRow) {
        return false;
    }
    let Some(container) = parent.downcast_ref::<gtk::Box>() else {
        return false;
    };
    match tag {
        Tag::ListItemIcon | Tag::ListItemAvatar => container.prepend(child),
        Tag::ListItemText => super::precede(container, child, Tag::ListItemAction),
        Tag::ListItemAction => container.append(child),
        _ => return false,
    }
    true
}
