//! Cards and the other framing surfaces, with the parts a card is built from.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Width a page body is limited to before it stops growing, in pixels. Long
/// lines are unreadable, so a container stops widening rather than filling a
/// maximised window.
const BODY_PIXELS: i32 = 720;

/// Surfaces that frame other components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Card | Tag::Paper => frame().upcast(),
        Tag::CardHeader => header().upcast(),
        Tag::CardContent => axis::column(8).upcast(),
        Tag::CardActions | Tag::AccordionActions => actions().upcast(),
        Tag::CardMedia => picture().upcast(),
        Tag::CardActionArea => area().upcast(),
        Tag::Container => container().upcast(),
        Tag::Section => axis::column(8).upcast(),
        Tag::Toolbar => toolbar().upcast(),
        Tag::HeaderBar => gtk::HeaderBar::new().upcast(),
        // Sidebar is the last surface tag routed here. It stays a catch-all
        // because `Tag` is one enum for every family and a family builder
        // cannot name the other hundred variants.
        _ => sidebar().upcast(),
    }
}

/// A framed surface holding a column, so the parts placed in it stack in the
/// order they were described rather than replacing one another.
fn frame() -> gtk::Frame {
    let widget = gtk::Frame::new(None);
    widget.set_hexpand(true);
    widget.set_child(Some(&axis::column(8)));
    widget
}

/// A title row: an icon, a title and a subtitle, each addressable as a slot.
fn header() -> gtk::Box {
    let strip = axis::row(8);
    let column = axis::column(0);
    column.set_hexpand(true);
    column.append(&slot::caption_label());
    column.append(&slot::detail_label());
    strip.append(&slot::emblem_image());
    strip.append(&column);
    strip
}

fn actions() -> gtk::Box {
    let widget = axis::row(6);
    widget.set_halign(gtk::Align::End);
    widget
}

fn picture() -> gtk::Picture {
    let widget = gtk::Picture::new();
    widget.set_can_shrink(true);
    widget.set_content_fit(gtk::ContentFit::Cover);
    widget.set_size_request(-1, 140);
    widget
}

/// A card body that is itself one large button.
fn area() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_has_frame(false);
    widget.set_hexpand(true);
    widget
}

fn container() -> gtk::Box {
    let widget = axis::column(12);
    widget.set_halign(gtk::Align::Center);
    widget.set_size_request(BODY_PIXELS, -1);
    widget
}

fn toolbar() -> gtk::Box {
    let widget = axis::row(6);
    widget.set_hexpand(true);
    widget
}

fn sidebar() -> gtk::Box {
    let widget = axis::column(2);
    widget.set_size_request(190, -1);
    widget
}

/// Places a card's own parts in the slots the frame keeps for them.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    let Some(frame) = parent.downcast_ref::<gtk::Frame>() else {
        return false;
    };
    if tag == Tag::CardHeader {
        // A frame's header is a real slot: it is drawn in the border rather
        // than above the content, which is what makes a card read as one
        // surface.
        frame.set_label_widget(Some(child));
        return true;
    }
    body(frame, child, tag)
}

/// A card's body stays above its action row, whichever was described first.
fn body(frame: &gtk::Frame, child: &gtk::Widget, tag: Tag) -> bool {
    if tag != Tag::CardContent && tag != Tag::CardMedia {
        return false;
    }
    let Some(column) = frame.child().and_then(|held| held.downcast::<gtk::Box>().ok()) else {
        return false;
    };
    super::precede(&column, child, Tag::CardActions);
    true
}

/// Attaches to a framed surface, which holds its content in a column, or to
/// the window chrome, which packs from its leading edge.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    if let Some(header) = parent.downcast_ref::<gtk::HeaderBar>() {
        header.pack_start(child);
        return true;
    }
    let Some(frame) = parent.downcast_ref::<gtk::Frame>() else {
        return false;
    };
    match frame.child().and_then(|held| held.downcast::<gtk::Box>().ok()) {
        Some(column) => column.append(child),
        None => frame.set_child(Some(child)),
    }
    true
}

/// Removes a part from a framed surface, including from its header slot.
pub(crate) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(frame) = parent.downcast_ref::<gtk::Frame>() else {
        return false;
    };
    if frame.label_widget().is_some_and(|held| held.eq(child)) {
        frame.set_label_widget(gtk::Widget::NONE);
        return true;
    }
    child.unparent();
    true
}
