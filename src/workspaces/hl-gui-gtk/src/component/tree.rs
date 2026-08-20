//! A hierarchy described as nodes, disclosed one level at a time.
//!
//! The hierarchy crosses the wire as described children: a tree holds items,
//! and an item holds further items. It is deliberately not built on
//! `gtk::TreeListModel`, because the windowed row protocol delivers flat rows
//! carrying neither a parent nor a depth — there is no model to expand from,
//! and a list model synthesized from described widgets would recycle the very
//! widgets the patch protocol addresses by identity.

use gtk::prelude::*;
use hl_gui::Tag;

use super::axis;

/// Space one level of depth adds to what an item holds, in pixels.
const INDENT_PIXELS: i32 = 16;
/// Height a tree asks for before it starts scrolling, in pixels.
const TRUNK_PIXELS: i32 = 160;

/// Tree components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Tree => trunk().upcast(),
        // TreeItem is the last tree tag routed here.
        _ => item().upcast(),
    }
}

/// The tree itself: a scroller over the column its top-level items stack in.
fn trunk() -> gtk::ScrolledWindow {
    let column = axis::column(0);
    column.set_hexpand(true);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&column));
    window.set_hexpand(true);
    window.set_vexpand(true);
    window.set_min_content_height(TRUNK_PIXELS);
    window
}

/// One node: a disclosure, so an item with children can be opened and an item
/// without them still reads as a line of the tree.
fn item() -> gtk::Expander {
    let widget = gtk::Expander::new(None);
    widget.set_hexpand(true);
    widget
}

/// The column a tree stacks its top-level items in, when the widget is a tree.
fn stem(widget: &gtk::Widget) -> Option<gtk::Box> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::Box>().ok())
}

/// Places what a tree and an item hold: top-level items in the trunk, and
/// everything an item holds in the indented body it discloses.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    if super::belongs(parent, Tag::Tree) {
        let Some(column) = stem(parent) else {
            return false;
        };
        column.append(child);
        return true;
    }
    if !super::belongs(parent, Tag::TreeItem) {
        return false;
    }
    let Some(expander) = parent.downcast_ref::<gtk::Expander>() else {
        return false;
    };
    branch(expander, child);
    true
}

/// Adds a child to what an item discloses, indenting the whole level rather
/// than each child — which is what makes depth readable at any nesting.
fn branch(expander: &gtk::Expander, child: &gtk::Widget) {
    if let Some(column) = expander.child().and_then(|held| held.downcast::<gtk::Box>().ok()) {
        column.append(child);
        return;
    }
    let column = axis::column(0);
    column.set_margin_start(INDENT_PIXELS);
    column.append(child);
    expander.set_child(Some(&column));
}
