//! A side panel that slides over the content it covers.
//!
//! GTK4 without libadwaita has no drawer, so the component is the two widgets
//! that give one honestly: an overlay, which is what makes the panel cover the
//! content rather than push it aside, and a revealer, which is what makes it
//! slide. The panel is a part of its own, because the covered content and the
//! thing covering it are different children with different slots.

use gtk::prelude::*;
use hl_gui::Tag;

use super::axis;

/// Width the panel occupies when it is open, in pixels.
const PANEL_PIXELS: i32 = 280;

/// Drawer components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Drawer => frame().upcast(),
        // DrawerPanel is the last drawer tag routed here.
        _ => panel().upcast(),
    }
}

/// The drawer: an overlay whose main child is the content, so anything the
/// panel is placed over is drawn beneath it.
fn frame() -> gtk::Overlay {
    let content = axis::column(0);
    content.set_hexpand(true);
    content.set_vexpand(true);
    let widget = gtk::Overlay::new();
    widget.set_child(Some(&content));
    widget.set_hexpand(true);
    widget.set_vexpand(true);
    widget
}

/// The panel: a revealer pinned to the leading edge, holding the column its
/// contents stack in. It is closed until a producer says otherwise.
fn panel() -> gtk::Revealer {
    let column = axis::column(8);
    column.set_size_request(PANEL_PIXELS, -1);
    column.set_vexpand(true);
    let widget = gtk::Revealer::new();
    widget.set_transition_type(gtk::RevealerTransitionType::SlideRight);
    widget.set_child(Some(&column));
    widget.set_halign(gtk::Align::Start);
    widget.set_valign(gtk::Align::Fill);
    widget
}

/// The column behind a drawer or a panel, whichever of the two the widget is.
fn held(widget: &gtk::Widget) -> Option<gtk::Box> {
    if let Some(overlay) = widget.downcast_ref::<gtk::Overlay>() {
        return overlay.child().and_then(|child| child.downcast::<gtk::Box>().ok());
    }
    widget
        .downcast_ref::<gtk::Revealer>()
        .and_then(gtk::Revealer::child)
        .and_then(|child| child.downcast::<gtk::Box>().ok())
}

/// Places a drawer's parts: the panel over the content whatever order the two
/// were described in, and everything else under it.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    if super::belongs(parent, Tag::DrawerPanel) {
        let Some(column) = held(parent) else {
            return false;
        };
        column.append(child);
        return true;
    }
    if !super::belongs(parent, Tag::Drawer) {
        return false;
    }
    if let Some(overlay) = parent
        .downcast_ref::<gtk::Overlay>()
        .filter(|_| tag == Tag::DrawerPanel)
    {
        // An overlay child, never the main one: a panel described first would
        // otherwise become the content it is supposed to cover.
        overlay.add_overlay(child);
        return true;
    }
    let Some(column) = held(parent) else {
        return false;
    };
    column.append(child);
    true
}
