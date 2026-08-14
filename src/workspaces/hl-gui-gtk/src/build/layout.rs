use gtk::prelude::*;
use hl_gui::Tag;

/// Containers and spacing primitives.
pub(super) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Column => gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(),
        Tag::Row => gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast(),
        Tag::Grid => gtk::Grid::new().upcast(),
        Tag::Scroll => scroll().upcast(),
        Tag::Splitter => gtk::Paned::new(gtk::Orientation::Horizontal).upcast(),
        Tag::Stack => gtk::Stack::new().upcast(),
        Tag::Overlay => gtk::Overlay::new().upcast(),
        Tag::Spacer => spacer().upcast(),
        _ => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
    }
}

fn scroll() -> gtk::ScrolledWindow {
    let window = gtk::ScrolledWindow::new();
    window.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    window.set_hexpand(true);
    window.set_vexpand(true);
    window
}

/// A blank box that consumes leftover horizontal space. Expanding on both axes
/// would stretch every toolbar it sits in; a vertical spacer asks for a height.
fn spacer() -> gtk::Box {
    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    widget.set_hexpand(true);
    widget
}

/// Attaches to the layout containers whose protocol is not `gtk::Box`.
pub(super) fn attach(parent: &gtk::Widget, child: &gtk::Widget, index: usize) -> bool {
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        if index == 0 {
            paned.set_start_child(Some(child));
        } else {
            paned.set_end_child(Some(child));
        }
        return true;
    }
    if let Some(stack) = parent.downcast_ref::<gtk::Stack>() {
        stack.add_named(child, Some(&format!("page-{index}")));
        return true;
    }
    if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
        if index == 0 {
            overlay.set_child(Some(child));
        } else {
            overlay.add_overlay(child);
        }
        return true;
    }
    if let Some(grid) = parent.downcast_ref::<gtk::Grid>() {
        let columns = grid.property::<i32>("column-spacing").max(1);
        let span = 3_i32.max(columns.min(4));
        grid.attach(child, index as i32 % span, index as i32 / span, 1, 1);
        return true;
    }
    false
}
