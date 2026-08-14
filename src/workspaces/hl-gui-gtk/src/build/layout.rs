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
        Tag::Stack => stack().upcast(),
        Tag::Overlay => gtk::Overlay::new().upcast(),
        Tag::Spacer => spacer().upcast(),
        // Separator is the last layout tag, and `build::widget` routes only
        // layout tags here, so this arm is that tag. It stays a catch-all
        // because `Tag` is one enum for every family and a family builder
        // cannot name the other fifty variants.
        _ => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
    }
}

/// One page visible at a time. The stack expands so a page is laid out at the
/// size the stack was given rather than at the size of the widest page.
fn stack() -> gtk::Stack {
    let widget = gtk::Stack::new();
    widget.set_hexpand(true);
    widget.set_vexpand(true);
    widget
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

/// Removes a child from the layout containers this module attaches to.
///
/// These containers keep their own page or slot bookkeeping, so unparenting a
/// child behind their back leaves a named page pointing at nothing — which is
/// exactly what makes a later move place the child twice.
pub(super) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    if let Some(stack) = parent.downcast_ref::<gtk::Stack>() {
        stack.remove(child);
        return true;
    }
    if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
        return uncover(overlay, child);
    }
    if let Some(grid) = parent.downcast_ref::<gtk::Grid>() {
        grid.remove(child);
        return true;
    }
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        return unpane(paned, child);
    }
    false
}

fn uncover(overlay: &gtk::Overlay, child: &gtk::Widget) -> bool {
    if overlay.child().is_some_and(|held| held.eq(child)) {
        overlay.set_child(gtk::Widget::NONE);
        return true;
    }
    overlay.remove_overlay(child);
    true
}

fn unpane(paned: &gtk::Paned, child: &gtk::Widget) -> bool {
    if paned.start_child().is_some_and(|held| held.eq(child)) {
        paned.set_start_child(gtk::Widget::NONE);
        return true;
    }
    paned.set_end_child(gtk::Widget::NONE);
    true
}
