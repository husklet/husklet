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
        // Menu and MenuItem are widget menus, not `gio::Menu` models: the model
        // API takes actions and labels, while these carry described children
        // and report through the adapter's own handler bindings.
        Tag::Menu => menu().upcast(),
        Tag::MenuItem => menu_item().upcast(),
        // Dialog is a box, not a `gtk::Window`. A detached tag still attaches to
        // the surface root, and a window cannot be a child of a widget, so the
        // embedder decides whether to present this subtree in a window.
        Tag::Dialog => gtk::Box::new(gtk::Orientation::Vertical, 12).upcast(),
        // Toast and Banner are the last surface tags, and `build::widget`
        // routes only surface tags here. Both live in libadwaita; a revealer
        // over an icon and a message gives the behavior — appear, carry text,
        // dismiss — without taking that dependency.
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

fn menu() -> gtk::Box {
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 2);
    widget.set_hexpand(true);
    widget
}

fn menu_item() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_has_frame(false);
    widget
}

/// The message strip behind Toast and Banner: an icon slot and a label, both
/// built up front so `Prop::Icon` and `Prop::Label` have somewhere to land.
fn notice() -> gtk::Revealer {
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let emblem = gtk::Image::new();
    let message = gtk::Label::new(None);
    message.set_halign(gtk::Align::Start);
    // Aligning the widget is not enough: a label that fills its slot still
    // centres its own text, so a notice would read from the middle.
    message.set_xalign(0.0);
    // Deliberately not expanding: a notice may also carry described children,
    // and an empty message slot that filled the strip would push them to the
    // far edge.
    message.set_wrap(true);
    strip.append(&emblem);
    strip.append(&message);
    let widget = gtk::Revealer::new();
    widget.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    widget.set_reveal_child(true);
    widget.set_child(Some(&strip));
    widget
}

/// The message label of a notice, when the widget is one.
/// It is the slot after the icon rather than the last child, because described
/// children are appended to the same strip and would otherwise take its place.
pub(crate) fn message(widget: &gtk::Widget) -> Option<gtk::Label> {
    strip(widget)?
        .first_child()
        .and_then(|slot| slot.next_sibling())
        .and_then(|slot| slot.downcast().ok())
}

/// The icon slot of a notice, when the widget is one.
pub(crate) fn emblem(widget: &gtk::Widget) -> Option<gtk::Image> {
    strip(widget)?.first_child().and_then(|slot| slot.downcast().ok())
}

fn strip(widget: &gtk::Widget) -> Option<gtk::Box> {
    widget
        .downcast_ref::<gtk::Revealer>()
        .and_then(gtk::Revealer::child)
        .and_then(|child| child.downcast::<gtk::Box>().ok())
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
pub(super) fn set_single(
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
