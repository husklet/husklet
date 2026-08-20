//! Every shape of invocation, from a plain button to a dial of actions.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Interactive controls that report an invocation.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Button => gtk::Button::new().upcast(),
        Tag::IconButton => icon().upcast(),
        Tag::ToggleButton => gtk::ToggleButton::new().upcast(),
        Tag::ButtonGroup | Tag::ToggleButtonGroup => group().upcast(),
        Tag::SplitButton => split().upcast(),
        Tag::Fab => floating().upcast(),
        Tag::SpeedDial | Tag::Overflow => dial().upcast(),
        Tag::SpeedDialAction | Tag::MenuItem | Tag::PaginationItem | Tag::TableSortLabel => axis::item().upcast(),
        // FilePicker is the last button tag routed here. GTK4 has no
        // file-chooser *widget* left: the chooser is `gtk::FileDialog`, which is
        // asynchronous and needs a parent window the adapter does not have at
        // construction. The button is therefore the component, and the embedder
        // opens the dialog from the invoke it reports.
        _ => gtk::Button::with_label("Choose…").upcast(),
    }
}

fn icon() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_icon_name("view-more-symbolic");
    widget
}

/// Buttons drawn as one control. GTK's own sheet gives `linked` the shared
/// border, so the grouping is styling rather than a container of its own.
fn group() -> gtk::Box {
    let widget = axis::row(0);
    widget.add_css_class("linked");
    widget.set_halign(gtk::Align::Start);
    widget
}

/// A default action beside the menu of its alternatives. The action carries the
/// component's own label, so the label lands in a slot rather than on the box.
fn split() -> gtk::Box {
    let widget = group();
    let action = gtk::Button::new();
    action.set_child(Some(&slot::caption_label()));
    widget.append(&action);
    widget.append(&dial());
    widget
}

fn floating() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_icon_name("list-add-symbolic");
    widget.add_css_class("circular");
    widget.set_halign(gtk::Align::End);
    widget
}

/// A button revealing further actions in a popover of its own.
///
/// The popover holds a column so the described actions stack in it; a menu
/// button otherwise takes a single child and would keep only the last one.
fn dial() -> gtk::MenuButton {
    let widget = gtk::MenuButton::new();
    widget.set_icon_name("view-more-symbolic");
    let popover = gtk::Popover::new();
    popover.set_child(Some(&axis::column(2)));
    widget.set_popover(Some(&popover));
    widget
}

/// The column a menu button reveals, which is where its children belong.
pub(crate) fn revealed(widget: &gtk::Widget) -> Option<gtk::Box> {
    widget
        .downcast_ref::<gtk::MenuButton>()
        .and_then(gtk::MenuButton::popover)
        .and_then(|popover| popover.child())
        .and_then(|column| column.downcast::<gtk::Box>().ok())
}

/// Attaches to the controls that hold a child: a menu button reveals its
/// children, and a button is a container in the component library — an icon
/// beside a label is described, not configured — which GTK4 agrees with, since
/// a button's label is only the child it builds by default.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    if let Some(column) = revealed(parent) {
        column.append(child);
        return true;
    }
    let Some(button) = parent.downcast_ref::<gtk::Button>() else {
        return false;
    };
    single(button.child(), child, |value| button.set_child(value))
}

/// Removes a child from the controls this module attaches to.
///
/// A button tracks its child itself, so unparenting behind its back leaves it
/// pointing at a widget it no longer holds.
pub(crate) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(button) = parent.downcast_ref::<gtk::Button>() else {
        return false;
    };
    if button.child().is_some_and(|held| held.eq(child)) {
        button.set_child(gtk::Widget::NONE);
        return true;
    }
    false
}

/// A single-child surface holds the first attachment and wraps later ones into
/// a column, so a producer is never silently limited to one child.
pub(crate) fn single(
    existing: Option<gtk::Widget>,
    child: &gtk::Widget,
    assign: impl Fn(Option<&gtk::Widget>),
) -> bool {
    match existing {
        None => assign(Some(child)),
        Some(current) if current.is::<gtk::Box>() => {
            current.downcast_ref::<gtk::Box>().expect("checked above").append(child);
        }
        Some(current) => {
            let column = axis::column(8);
            assign(None);
            column.append(&current);
            column.append(child);
            assign(Some(column.upcast_ref::<gtk::Widget>()));
        }
    }
    true
}
