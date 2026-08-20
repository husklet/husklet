//! Dialogs, menus and the surfaces that appear over everything else.

use gtk::prelude::*;
use hl_gui::Tag;

use super::axis;

/// Transient surfaces and their parts.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        // Dialog is a box, not a `gtk::Window`. A detached tag still attaches to
        // the surface root, and a window cannot be a child of a widget, so the
        // embedder decides whether to present this subtree in a window.
        Tag::Dialog => body().upcast(),
        Tag::DialogTitle => title().upcast(),
        Tag::DialogContent => content().upcast(),
        Tag::DialogContentText => prose().upcast(),
        Tag::DialogActions => actions().upcast(),
        Tag::Popover | Tag::ContextMenu => gtk::Popover::new().upcast(),
        // Menu is the last dialog tag routed here. Menus here are widget menus,
        // not `gio::Menu` models: the model API takes actions and labels, while
        // these carry described children and report through the adapter's own
        // handler bindings.
        _ => menu().upcast(),
    }
}

fn body() -> gtk::Box {
    let widget = axis::column(12);
    widget.set_hexpand(true);
    widget
}

fn title() -> gtk::Label {
    let widget = axis::label();
    widget.add_css_class("title-2");
    widget
}

fn content() -> gtk::Box {
    let widget = axis::column(8);
    widget.set_vexpand(true);
    widget
}

fn prose() -> gtk::Label {
    let widget = axis::label();
    widget.set_wrap(true);
    widget
}

fn actions() -> gtk::Box {
    let widget = axis::row(6);
    widget.set_halign(gtk::Align::End);
    widget
}

fn menu() -> gtk::Box {
    let widget = axis::column(2);
    widget.set_hexpand(true);
    widget
}

/// Places a dialog's parts: the title above whatever the dialog says, the
/// actions below it, whichever order they were described in.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    if !super::belongs(parent, Tag::Dialog) {
        return false;
    }
    let Some(container) = parent.downcast_ref::<gtk::Box>() else {
        return false;
    };
    match tag {
        Tag::DialogTitle => container.prepend(child),
        Tag::DialogContent => super::precede(container, child, Tag::DialogActions),
        Tag::DialogActions => container.append(child),
        _ => return false,
    }
    true
}

/// Attaches to a popover, which holds one child and so is given a column.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(popover) = parent.downcast_ref::<gtk::Popover>() else {
        return false;
    };
    match popover.child().and_then(|held| held.downcast::<gtk::Box>().ok()) {
        Some(column) => column.append(child),
        None => fill(popover, child),
    }
    true
}

/// Removes a child from a popover, which tracks the child it holds.
pub(crate) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(popover) = parent.downcast_ref::<gtk::Popover>() else {
        return false;
    };
    if popover.child().is_some_and(|held| held.eq(child)) {
        popover.set_child(gtk::Widget::NONE);
        return true;
    }
    false
}

fn fill(popover: &gtk::Popover, child: &gtk::Widget) {
    let column = axis::column(2);
    column.append(child);
    popover.set_child(Some(&column));
}
