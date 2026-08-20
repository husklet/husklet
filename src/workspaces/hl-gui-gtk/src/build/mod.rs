//! Widget construction and placement.
//!
//! Construction dispatches to the component family that owns a tag; placement
//! asks that family where the child belongs before falling back to the plain
//! container protocols.

mod flow;
pub(crate) mod layout;

use gtk::prelude::*;
use hl_gui::{Node, Tag};

use crate::component;

/// Builds the widget for a freshly created node.
///
/// Construction only; properties arrive as separate patches, so this stays a
/// dispatch rather than a per-tag configuration routine.
pub(crate) fn widget(node: &Node) -> gtk::Widget {
    let built = match node.tag {
        Tag::Column
        | Tag::Row
        | Tag::Grid
        | Tag::Scroll
        | Tag::Splitter
        | Tag::Stack
        | Tag::Overlay
        | Tag::Spacer
        | Tag::Separator => layout::widget(node.tag),
        _ => component::widget(node.tag),
    };
    built.add_css_class(class(node.tag).as_str());
    built
}

/// Stable per-tag style class, so the generated sheet can target a component
/// family without the adapter emitting inline rules.
pub(crate) fn class(tag: Tag) -> String {
    format!("hl-{}", tag.as_str().to_ascii_lowercase())
}

/// Attaches `child` to `parent` at `index`.
///
/// The child's own tag is asked first, because a part belongs in the slot its
/// parent keeps for it — a card header in the card's header, a dialog's actions
/// at the foot of the dialog — and only then does the parent's plain container
/// protocol decide.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag, index: usize) -> bool {
    if component::attach(parent, child, tag, index) {
        return true;
    }
    if let Some(container) = parent.downcast_ref::<gtk::Box>() {
        insert_into(container, child, index);
        return true;
    }
    if let Some(window) = parent.downcast_ref::<gtk::ScrolledWindow>() {
        window.set_child(Some(child));
        return true;
    }
    layout::attach(parent, child, index)
}

fn insert_into(container: &gtk::Box, child: &gtk::Widget, index: usize) {
    let sibling = container.first_child();
    let mut cursor = sibling;
    let mut position = 0;
    while position < index {
        let Some(current) = cursor else { break };
        cursor = current.next_sibling();
        position += 1;
    }
    match cursor {
        Some(next) => container.insert_child_after(child, next.prev_sibling().as_ref()),
        None => container.append(child),
    }
}

/// Detaches a child from whichever container currently holds it.
pub(crate) fn detach(child: &gtk::Widget) {
    let Some(parent) = child.parent() else {
        return;
    };
    if let Some(container) = parent.downcast_ref::<gtk::Box>() {
        container.remove(child);
        return;
    }
    if let Some(window) = parent.downcast_ref::<gtk::ScrolledWindow>() {
        window.set_child(gtk::Widget::NONE);
        return;
    }
    if component::detach(&parent, child) || layout::detach(&parent, child) {
        return;
    }
    child.unparent();
}
