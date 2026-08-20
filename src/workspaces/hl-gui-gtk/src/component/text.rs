//! Text, imagery and the small marks that stand beside them.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Edge length of a monogram, in pixels.
const MONOGRAM_PIXELS: i32 = 36;
/// Pictures shown side by side before an image list wraps.
const GALLERY_COLUMNS: u32 = 4;

/// Non-interactive presentation widgets.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Text | Tag::ListSubheader | Tag::FormLabel => axis::label().upcast(),
        Tag::Heading => heading().upcast(),
        Tag::Code => code().upcast(),
        Tag::Link => gtk::LinkButton::new("").upcast(),
        Tag::Icon | Tag::ListItemIcon | Tag::StepIcon => gtk::Image::from_icon_name("image-missing-symbolic").upcast(),
        Tag::Badge => badge().upcast(),
        Tag::Avatar | Tag::ListItemAvatar => avatar().upcast(),
        Tag::AvatarGroup => axis::row(0).upcast(),
        Tag::Chip => chip().upcast(),
        Tag::Image | Tag::ImageListItem => picture().upcast(),
        // ImageList is the last display tag routed here.
        _ => gallery().upcast(),
    }
}

fn heading() -> gtk::Label {
    let widget = axis::label();
    widget.add_css_class("title-3");
    widget
}

fn code() -> gtk::Label {
    let widget = axis::label();
    widget.add_css_class("monospace");
    widget.set_selectable(true);
    widget
}

/// A compact status chip. GTK has no badge widget; a styled label is the
/// idiomatic equivalent and keeps the sheet in charge of appearance.
fn badge() -> gtk::Label {
    let widget = gtk::Label::new(None);
    widget.set_halign(gtk::Align::Start);
    widget.set_valign(gtk::Align::Center);
    widget
}

/// A round monogram. libadwaita owns the real one, so this is a circular label
/// sized by the sheet rather than a dependency on that library.
fn avatar() -> gtk::Label {
    let widget = gtk::Label::new(None);
    widget.set_halign(gtk::Align::Center);
    widget.set_valign(gtk::Align::Center);
    widget.set_size_request(MONOGRAM_PIXELS, MONOGRAM_PIXELS);
    widget
}

/// A token: an icon slot, its own text, and room for the control that removes
/// it, which arrives as a described child.
fn chip() -> gtk::Box {
    let widget = axis::row(4);
    widget.set_halign(gtk::Align::Start);
    widget.append(&slot::emblem_image());
    widget.append(&slot::caption_label());
    widget
}

/// A picture that may shrink below its natural size, or an image larger than
/// its slot forces the whole surface wider instead of scaling down.
fn picture() -> gtk::Picture {
    let widget = gtk::Picture::new();
    widget.set_can_shrink(true);
    widget.set_content_fit(gtk::ContentFit::Contain);
    widget
}

/// A wrapping grid of pictures.
fn gallery() -> gtk::FlowBox {
    let widget = gtk::FlowBox::new();
    widget.set_selection_mode(gtk::SelectionMode::None);
    widget.set_max_children_per_line(GALLERY_COLUMNS);
    widget.set_homogeneous(true);
    widget.set_hexpand(true);
    widget
}

/// Attaches to the wrapping grid, which wraps every child of its own accord.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(gallery) = parent.downcast_ref::<gtk::FlowBox>() else {
        return false;
    };
    gallery.append(child);
    true
}

/// Removes a picture from the wrapping grid.
///
/// A flow box parents each child inside a wrapper of its own, so the child's
/// parent is that wrapper and unparenting the child leaves the wrapper behind.
pub(crate) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(wrapper) = parent.downcast_ref::<gtk::FlowBoxChild>() else {
        return false;
    };
    let Some(gallery) = wrapper.parent().and_then(|held| held.downcast::<gtk::FlowBox>().ok()) else {
        return false;
    };
    child.unparent();
    gallery.remove(wrapper);
    true
}
