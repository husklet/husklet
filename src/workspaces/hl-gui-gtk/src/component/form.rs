//! The frame around a field, and the controls a choice is made with.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

/// Form structure and choice controls.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::FormControl | Tag::FormGroup => axis::column(4).upcast(),
        Tag::FormHelperText => helper().upcast(),
        Tag::FormControlLabel => caption().upcast(),
        Tag::Switch => switch().upcast(),
        Tag::Checkbox | Tag::Radio => gtk::CheckButton::new().upcast(),
        Tag::RadioGroup => axis::column(4).upcast(),
        // Select is the last form tag routed here.
        _ => gtk::DropDown::from_strings(&[]).upcast(),
    }
}

fn helper() -> gtk::Label {
    let widget = axis::label();
    widget.add_css_class("dim-label");
    widget.set_wrap(true);
    widget
}

/// A control with its caption beside it. The control arrives as a child and is
/// placed before the caption, which is where a person expects to find it.
fn caption() -> gtk::Box {
    let widget = axis::row(8);
    widget.set_valign(gtk::Align::Center);
    let caption = slot::caption_label();
    caption.set_wrap(true);
    caption.set_xalign(0.0);
    caption.set_hexpand(true);
    widget.append(&caption);
    widget
}

fn switch() -> gtk::Switch {
    let widget = gtk::Switch::new();
    widget.set_halign(gtk::Align::Start);
    widget.set_valign(gtk::Align::Center);
    widget
}

/// Attaches an option to a group, which is what makes the choice exclusive.
///
/// GTK4 has no radio widget: a check button becomes a radio by joining another
/// one's group, so the grouping has to happen where the option is placed.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    if tag != Tag::Radio || !super::belongs(parent, Tag::RadioGroup) {
        return false;
    }
    let Some(container) = parent.downcast_ref::<gtk::Box>() else {
        return false;
    };
    let Some(option) = child.downcast_ref::<gtk::CheckButton>() else {
        return false;
    };
    option.set_group(first(container).as_ref());
    container.append(child);
    true
}

/// The option already in a group, which every later one joins.
fn first(container: &gtk::Box) -> Option<gtk::CheckButton> {
    container
        .first_child()
        .and_then(|child| child.downcast::<gtk::CheckButton>().ok())
}
