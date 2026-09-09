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
        _ => super::choice::widget().upcast(),
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
    let enter = gtk::EventControllerKey::new();
    let target = widget.clone();
    enter.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter || key == gtk::gdk::Key::space {
            if target.is_sensitive() {
                request_switch_toggle(&target);
            }
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    widget.add_controller(enter);
    widget
}

/// Attaches an option to a group, which is what makes the choice exclusive.
///
/// GTK4 has no radio widget: a check button becomes a radio by joining another
/// one's group, so the grouping has to happen where the option is placed.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
    if super::belongs(parent, Tag::FormControl) {
        let Some(container) = parent.downcast_ref::<gtk::Box>() else {
            return false;
        };
        container.append(child);
        associate(container);
        return true;
    }
    if super::belongs(parent, Tag::FormControlLabel) {
        let Some(container) = parent.downcast_ref::<gtk::Box>() else {
            return false;
        };
        slot::field(child);
        if let Some(caption) = slot::caption(parent) {
            if child.is_focusable() {
                caption.set_mnemonic_widget(Some(child));
            }
            child.update_relation(&[gtk::accessible::Relation::LabelledBy(&[caption.upcast_ref()])]);
        }
        container.prepend(child);
        if let Some(choice) = child.downcast_ref::<gtk::Switch>() {
            let click = gtk::GestureClick::new();
            let row = container.clone();
            let choice = choice.clone();
            click.connect_released(move |_, _, x, y| {
                let over_choice = choice
                    .compute_bounds(&row)
                    .is_some_and(|bounds| bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)));
                if !over_choice && choice.is_sensitive() {
                    request_switch_toggle(&choice);
                    choice.grab_focus();
                }
            });
            container.add_controller(click);
        }
        return true;
    }
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

fn request_switch_toggle(widget: &gtk::Switch) {
    widget.emit_by_name::<bool>("state-set", &[&!widget.is_active()]);
}

/// Connects the authored label and helper to the focusable field they explain.
fn associate(container: &gtk::Box) {
    let children = slot::offspring(container.upcast_ref());
    // A freshly constructed field is attached before GTK has rooted it and can
    // still report `is_focusable() == false`. Its component identity is stable
    // at construction, so prefer that over transient toolkit state.
    let field = children
        .iter()
        .find(|child| super::belongs(child, Tag::Entry) || super::belongs(child, Tag::Select))
        .or_else(|| children.iter().find(|child| child.is_focusable()));
    let label = children
        .iter()
        .find(|child| super::belongs(child, Tag::FormLabel))
        .and_then(|child| child.downcast_ref::<gtk::Label>());
    let helper = children
        .iter()
        .find(|child| super::belongs(child, Tag::FormHelperText))
        .and_then(|child| child.downcast_ref::<gtk::Label>());
    let Some(field) = field else { return };
    if let Some(label) = label {
        label.set_mnemonic_widget(Some(field));
        field.update_relation(&[gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()])]);
    }
    if let Some(helper) = helper {
        field.update_relation(&[gtk::accessible::Relation::DescribedBy(&[helper.upcast_ref()])]);
    }
}

/// The option already in a group, which every later one joins.
fn first(container: &gtk::Box) -> Option<gtk::CheckButton> {
    container
        .first_child()
        .and_then(|child| child.downcast::<gtk::CheckButton>().ok())
}
