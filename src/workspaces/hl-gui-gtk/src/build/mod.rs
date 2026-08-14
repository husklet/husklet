//! Widget construction. One function per component family, dispatched by tag.

pub(crate) mod collection;
pub(crate) mod control;
mod display;
mod layout;
mod surface;

use gtk::prelude::*;
use hl_gui::{Node, Tag};

/// Builds the widget for a freshly created node.
///
/// Construction only; properties arrive as separate patches, so this stays a
/// flat dispatch table rather than a per-tag configuration routine.
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
        Tag::Text
        | Tag::Heading
        | Tag::Code
        | Tag::Link
        | Tag::Icon
        | Tag::Badge
        | Tag::Avatar
        | Tag::Progress
        | Tag::Spinner
        | Tag::Image => display::widget(node.tag),
        Tag::Button
        | Tag::IconButton
        | Tag::ToggleButton
        | Tag::Entry
        | Tag::Search
        | Tag::NumberEntry
        | Tag::TextArea
        | Tag::Switch
        | Tag::Checkbox
        | Tag::RadioGroup
        | Tag::Select
        | Tag::Slider
        | Tag::DatePicker
        | Tag::ColorPicker
        | Tag::FilePicker => control::widget(node.tag),
        Tag::Card
        | Tag::Section
        | Tag::Toolbar
        | Tag::HeaderBar
        | Tag::Sidebar
        | Tag::Tabs
        | Tag::TabPage
        | Tag::Expander
        | Tag::Popover
        | Tag::Menu
        | Tag::MenuItem
        | Tag::Dialog
        | Tag::Toast
        | Tag::Banner => surface::widget(node.tag),
        Tag::List | Tag::ListRow | Tag::DataTable | Tag::TreeTable | Tag::Chart => collection::widget(node.tag),
    };
    built.add_css_class(class(node.tag).as_str());
    built
}

/// Stable per-tag style class, so the generated sheet can target a component
/// family without the adapter emitting inline rules.
pub(crate) fn class(tag: Tag) -> String {
    format!("hl-{}", tag.as_str().to_ascii_lowercase())
}

/// Attaches `child` to `parent` at `index`, using whichever GTK container
/// protocol the parent widget implements.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget, index: usize) -> bool {
    if let Some(container) = parent.downcast_ref::<gtk::Box>() {
        insert_into(container, child, index);
        return true;
    }
    if let Some(window) = parent.downcast_ref::<gtk::ScrolledWindow>() {
        window.set_child(Some(child));
        return true;
    }
    surface::attach(parent, child, index) || layout::attach(parent, child, index)
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
    if surface::detach(&parent, child) {
        return;
    }
    child.unparent();
}
