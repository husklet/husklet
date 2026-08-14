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
        _ => gtk::Button::with_label("Choose…").upcast(),
    }
}

fn icon_button() -> gtk::Button {
    let widget = gtk::Button::new();
    widget.set_icon_name("view-more-symbolic");
    widget
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

/// The editable buffer behind a text area, when the widget is one.
pub(crate) fn view(widget: &gtk::Widget) -> Option<gtk::TextView> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::TextView>().ok())
}
