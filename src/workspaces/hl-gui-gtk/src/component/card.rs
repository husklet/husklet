//! Cards and the other framing surfaces, with the parts a card is built from.

use std::cell::OnceCell;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Width a page body is limited to before it stops growing, in pixels. Long
/// lines are unreadable, so a container stops widening rather than filling a
/// maximised window.
const BODY_PIXELS: i32 = 984;

/// A page body whose preferred width is a ceiling rather than a minimum.
///
/// `set_size_request` cannot express this: a fixed request prevents GTK
/// from shrinking the body in a narrow pane. Reporting no horizontal minimum
/// and a bounded natural width gives the parent the intended contract instead:
/// use every available pixel up to that ceiling, then centre the body.
#[derive(Default)]
struct Body {
    column: OnceCell<gtk::Box>,
}

#[glib::object_subclass]
impl ObjectSubclass for Body {
    const NAME: &'static str = "HlPageBody";
    type Type = PageBody;
    type ParentType = gtk::Widget;
}

impl ObjectImpl for Body {
    fn constructed(&self) {
        self.parent_constructed();
        let column = axis::column(12);
        column.set_parent(&*self.obj());
        self.column.set(column).expect("page body constructed once");
    }

    fn dispose(&self) {
        if let Some(column) = self.column.get() {
            column.unparent();
        }
    }
}

impl WidgetImpl for Body {
    fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
        if orientation == gtk::Orientation::Horizontal {
            (0, BODY_PIXELS, -1, -1)
        } else {
            self.column().measure(orientation, for_size)
        }
    }

    fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
        let column = self.column();
        column.measure(gtk::Orientation::Horizontal, -1);
        let (minimum_height, _, _, _) = column.measure(gtk::Orientation::Vertical, width);
        column.allocate(width, height.max(minimum_height), baseline, None);
    }
}

impl Body {
    fn column(&self) -> &gtk::Box {
        self.column.get().expect("page body is constructed")
    }
}

glib::wrapper! {
    struct PageBody(ObjectSubclass<Body>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Surfaces that frame other components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Card | Tag::Paper => frame().upcast(),
        Tag::CardHeader => header().upcast(),
        Tag::CardContent => content().upcast(),
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
    widget.set_hexpand(false);
    widget.set_halign(gtk::Align::Start);
    // Card content may expand inside a row whose peers establish a taller line,
    // but that internal policy must not make a standalone card consume all
    // vertical space offered by a page.
    widget.set_vexpand(false);
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

/// The content owns any spare card height so a trailing action row stays
/// anchored to the card edge when a grid equalizes neighboring cards.
fn content() -> gtk::Box {
    let widget = axis::column(8);
    widget.set_vexpand(true);
    widget
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

fn container() -> PageBody {
    let widget: PageBody = glib::Object::new();
    widget.set_halign(gtk::Align::Center);
    widget
}

/// Whether this widget is the page-body clamp, whose children are parented
/// directly and laid out by its vertical box layout.
pub(crate) fn container_column(widget: &gtk::Widget) -> Option<gtk::Box> {
    widget
        .downcast_ref::<PageBody>()
        .map(|body| body.imp().column().clone())
}

pub(crate) fn gap(widget: &gtk::Widget, pixels: i32) -> bool {
    let Some(container) = container_column(widget) else {
        return false;
    };
    container.set_spacing(pixels);
    true
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
        frame.set_label_align(0.0);
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
