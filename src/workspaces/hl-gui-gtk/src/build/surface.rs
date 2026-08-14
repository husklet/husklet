use gtk::prelude::*;
use hl_gui::Tag;

/// Framing, chrome, and transient surfaces.
pub(super) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Card => card().upcast(),
        Tag::Section => gtk::Box::new(gtk::Orientation::Vertical, 8).upcast(),
        Tag::Toolbar => toolbar().upcast(),
        Tag::HeaderBar => gtk::HeaderBar::new().upcast(),
        Tag::Sidebar => sidebar().upcast(),
        Tag::Tabs => gtk::Notebook::new().upcast(),
        Tag::TabPage => gtk::Box::new(gtk::Orientation::Vertical, 8).upcast(),
        Tag::Expander => gtk::Expander::new(None).upcast(),
        Tag::Popover => gtk::Popover::new().upcast(),
        Tag::Menu => gtk::Box::new(gtk::Orientation::Vertical, 2).upcast(),
        Tag::MenuItem => menu_item().upcast(),
        Tag::Dialog => gtk::Box::new(gtk::Orientation::Vertical, 12).upcast(),
        // Toast and Banner live in libadwaita; a revealer over the same box
        // model gives the behavior without taking that dependency.
        _ => notice().upcast(),
    }
}

fn card() -> gtk::Frame {
    let widget = gtk::Frame::new(None);
    widget.set_hexpand(true);
    widget
}

fn toolbar() -> gtk::Box {
    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    widget.set_hexpand(true);
    widget
}

fn sidebar() -> gtk::Box {
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 2);
    widget.set_size_request(190, -1);
    widget
}

fn menu_item() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_has_frame(false);
    widget
}

fn notice() -> gtk::Revealer {
    let widget = gtk::Revealer::new();
    widget.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    widget.set_reveal_child(true);
    widget.set_child(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 8)));
    widget
}

/// Attaches to the surfaces whose protocol is not `gtk::Box`.
pub(super) fn attach(parent: &gtk::Widget, child: &gtk::Widget, index: usize) -> bool {
    if let Some(frame) = parent.downcast_ref::<gtk::Frame>() {
        return set_single(frame.child(), child, |value| frame.set_child(value));
    }
    if let Some(expander) = parent.downcast_ref::<gtk::Expander>() {
        expander.set_child(Some(child));
        return true;
    }
    if let Some(popover) = parent.downcast_ref::<gtk::Popover>() {
        popover.set_child(Some(child));
        return true;
    }
    if let Some(revealer) = parent.downcast_ref::<gtk::Revealer>() {
        return reveal(revealer, child);
    }
    if let Some(notebook) = parent.downcast_ref::<gtk::Notebook>() {
        let title = gtk::Label::new(Some(&format!("Tab {}", index + 1)));
        notebook.append_page(child, Some(&title));
        return true;
    }
    if let Some(header) = parent.downcast_ref::<gtk::HeaderBar>() {
        header.pack_start(child);
        return true;
    }
    false
}

/// A single-child surface holds the first attachment and wraps later ones into
/// a column, so a producer is never silently limited to one child.
fn set_single(existing: Option<gtk::Widget>, child: &gtk::Widget, assign: impl Fn(Option<&gtk::Widget>)) -> bool {
    match existing {
        None => assign(Some(child)),
        Some(current) if current.is::<gtk::Box>() => {
            current.downcast_ref::<gtk::Box>().expect("checked above").append(child);
        }
        Some(current) => {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
            assign(None);
            column.append(&current);
            column.append(child);
            assign(Some(column.upcast_ref::<gtk::Widget>()));
        }
    }
    true
}

fn reveal(revealer: &gtk::Revealer, child: &gtk::Widget) -> bool {
    match revealer.child().and_then(|slot| slot.downcast::<gtk::Box>().ok()) {
        Some(slot) => slot.append(child),
        None => revealer.set_child(Some(child)),
    }
    true
}

/// Removes a child from the surfaces this module attaches to.
pub(super) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    if let Some(notebook) = parent.downcast_ref::<gtk::Notebook>() {
        if let Some(page) = notebook.page_num(child) {
            notebook.remove_page(Some(page));
        }
        return true;
    }
    if parent.is::<gtk::Frame>() || parent.is::<gtk::Expander>() || parent.is::<gtk::Popover>() {
        child.unparent();
        return true;
    }
    false
}
