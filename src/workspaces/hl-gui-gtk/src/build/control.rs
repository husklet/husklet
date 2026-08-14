use gtk::prelude::*;
use hl_gui::Tag;

/// Interactive controls. Values and choices arrive as properties.
pub(super) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Button => gtk::Button::new().upcast(),
        Tag::IconButton => icon_button().upcast(),
        Tag::ToggleButton => gtk::ToggleButton::new().upcast(),
        Tag::Entry => gtk::Entry::new().upcast(),
        Tag::Search => gtk::SearchEntry::new().upcast(),
        Tag::NumberEntry => number().upcast(),
        Tag::TextArea => text_area().upcast(),
        Tag::Switch => switch().upcast(),
        Tag::Checkbox => gtk::CheckButton::new().upcast(),
        Tag::RadioGroup => gtk::Box::new(gtk::Orientation::Vertical, 4).upcast(),
        Tag::Select => gtk::DropDown::from_strings(&[]).upcast(),
        Tag::Slider => slider().upcast(),
        Tag::DatePicker => gtk::Calendar::new().upcast(),
        Tag::ColorPicker => gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new())).upcast(),
        // FilePicker is the last control tag, and `build::widget` routes only
        // control tags here. GTK4 has no file-chooser *widget* left: the
        // chooser is `gtk::FileDialog`, which is asynchronous and needs a
        // parent window the adapter does not have at construction. The button
        // is therefore the component, and the embedder opens the dialog from
        // the invoke it reports.
        _ => picker().upcast(),
    }
}

fn icon_button() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_icon_name("view-more-symbolic");
    widget
}

/// The label is a default, not a fixed caption: `Prop::Label` replaces it.
fn picker() -> gtk::Button {
    gtk::Button::with_label("Choose…")
}

fn number() -> gtk::SpinButton {
    let adjustment = gtk::Adjustment::new(0.0, 0.0, 100.0, 1.0, 10.0, 0.0);
    gtk::SpinButton::new(Some(&adjustment), 1.0, 0)
}

/// A multi-line editor is a view plus its scroller; the pair is the component.
fn text_area() -> gtk::ScrolledWindow {
    let view = gtk::TextView::new();
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_monospace(true);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&view));
    window.set_min_content_height(96);
    window.set_hexpand(true);
    window
}

fn switch() -> gtk::Switch {
    let widget = gtk::Switch::new();
    widget.set_halign(gtk::Align::Start);
    widget.set_valign(gtk::Align::Center);
    widget
}

fn slider() -> gtk::Scale {
    let widget = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    widget.set_hexpand(true);
    widget.set_draw_value(true);
    widget
}

/// Attaches to the controls that hold a child. A button is a container in the
/// component library — an icon beside a label is described, not configured —
/// and GTK4 agrees: its label is only the child it builds by default.
pub(super) fn attach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(button) = parent.downcast_ref::<gtk::Button>() else {
        return false;
    };
    super::surface::set_single(button.child(), child, |value| button.set_child(value))
}

/// Removes a child from the controls this module attaches to.
///
/// A button tracks its child itself, so unparenting behind its back leaves it
/// pointing at a widget it no longer holds.
pub(super) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(button) = parent.downcast_ref::<gtk::Button>() else {
        return false;
    };
    if button.child().is_some_and(|held| held.eq(child)) {
        button.set_child(gtk::Widget::NONE);
        return true;
    }
    false
}

/// The editable buffer behind a text area, when the widget is one.
pub(crate) fn view(widget: &gtk::Widget) -> Option<gtk::TextView> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::TextView>().ok())
}
