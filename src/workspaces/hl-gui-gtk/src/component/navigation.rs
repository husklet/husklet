//! Moving between places, and through a sequence of steps.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Width of a rail of destinations, in pixels.
const RAIL_PIXELS: i32 = 72;

/// Navigation components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Tabs => gtk::Notebook::new().upcast(),
        Tag::TabPage | Tag::Step | Tag::AccordionDetails => axis::column(8).upcast(),
        Tag::Breadcrumb | Tag::Pagination => trail(4).upcast(),
        Tag::Stepper => trail(12).upcast(),
        Tag::StepLabel | Tag::AccordionSummary => summary().upcast(),
        Tag::StepContent => indent().upcast(),
        Tag::StepConnector => connector().upcast(),
        Tag::NavigationRail => rail().upcast(),
        Tag::NavigationRailItem | Tag::BottomNavigationAction => destination().upcast(),
        Tag::BottomNavigation => bar().upcast(),
        // Accordion and Expander are the last navigation tags routed here: both
        // are one disclosure, and an accordion is the disclosure that names its
        // summary and its details as parts.
        _ => gtk::Expander::new(None).upcast(),
    }
}

fn trail(spacing: i32) -> gtk::Box {
    let widget = axis::row(spacing);
    widget.set_halign(gtk::Align::Start);
    widget
}

/// The always-visible line of a step or a disclosure: a marker and a caption.
fn summary() -> gtk::Box {
    let widget = axis::row(6);
    widget.set_hexpand(true);
    widget.append(&slot::emblem_image());
    widget.append(&slot::caption_label());
    widget
}

/// The body of a step, indented under the marker it belongs to.
fn indent() -> gtk::Box {
    let widget = axis::column(4);
    widget.set_margin_start(24);
    widget
}

fn connector() -> gtk::Separator {
    let widget = gtk::Separator::new(gtk::Orientation::Horizontal);
    widget.set_hexpand(true);
    widget.set_valign(gtk::Align::Center);
    widget
}

fn rail() -> gtk::Box {
    let widget = axis::column(4);
    widget.set_size_request(RAIL_PIXELS, -1);
    widget
}

fn destination() -> gtk::Button {
    let widget = axis::item();
    widget.set_hexpand(true);
    widget
}

/// Destinations across an edge, each given the same share of the width.
fn bar() -> gtk::Box {
    let widget = axis::row(0);
    widget.set_homogeneous(true);
    widget.set_hexpand(true);
    widget
}

/// Places the parts of a disclosure and of a step.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    if let Some(expander) = parent.downcast_ref::<gtk::Expander>() {
        return disclose(expander, child, tag);
    }
    if !super::belongs(parent, Tag::Step) {
        return false;
    }
    let Some(container) = parent.downcast_ref::<gtk::Box>() else {
        return false;
    };
    match tag {
        Tag::StepLabel => container.prepend(child),
        Tag::StepContent => container.append(child),
        _ => return false,
    }
    true
}

/// A disclosure keeps its summary in the header GTK draws beside the arrow,
/// and everything else in the body it reveals.
fn disclose(expander: &gtk::Expander, child: &gtk::Widget, tag: Tag) -> bool {
    if tag == Tag::AccordionSummary {
        expander.set_label_widget(Some(child));
        return true;
    }
    match expander.child().and_then(|held| held.downcast::<gtk::Box>().ok()) {
        Some(column) => column.append(child),
        None => body(expander, child),
    }
    true
}

/// The first thing revealed becomes the body; anything later joins it, so a
/// disclosure is never silently limited to one child.
fn body(expander: &gtk::Expander, child: &gtk::Widget) {
    let column = axis::column(8);
    column.append(child);
    expander.set_child(Some(&column));
}

/// Attaches to the tab strip, where every child becomes a page.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget, index: usize) -> bool {
    let Some(notebook) = parent.downcast_ref::<gtk::Notebook>() else {
        return false;
    };
    let title = gtk::Label::new(Some(&format!("Tab {}", index + 1)));
    notebook.append_page(child, Some(&title));
    true
}

/// Removes a part from a disclosure or a page from the tab strip.
pub(crate) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    if let Some(notebook) = parent.downcast_ref::<gtk::Notebook>() {
        if let Some(page) = notebook.page_num(child) {
            notebook.remove_page(Some(page));
        }
        return true;
    }
    let Some(expander) = parent.downcast_ref::<gtk::Expander>() else {
        return false;
    };
    if expander.label_widget().is_some_and(|held| held.eq(child)) {
        expander.set_label_widget(gtk::Widget::NONE);
        return true;
    }
    child.unparent();
    true
}
