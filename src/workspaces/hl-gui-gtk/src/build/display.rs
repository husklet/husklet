use gtk::prelude::*;
use hl_gui::Tag;

/// Non-interactive presentation widgets.
pub(super) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Text => label(gtk::Align::Start).upcast(),
        Tag::Heading => heading().upcast(),
        Tag::Code => code().upcast(),
        Tag::Link => gtk::LinkButton::new("").upcast(),
        Tag::Icon => gtk::Image::from_icon_name("image-missing-symbolic").upcast(),
        Tag::Badge => badge().upcast(),
        Tag::Avatar => avatar().upcast(),
        Tag::Progress => gtk::ProgressBar::new().upcast(),
        Tag::Spinner => spinner().upcast(),
        _ => gtk::Picture::new().upcast(),
    }
}

fn label(align: gtk::Align) -> gtk::Label {
    let widget = gtk::Label::new(None);
    widget.set_halign(align);
    widget.set_xalign(0.0);
    widget
}

fn heading() -> gtk::Label {
    let widget = label(gtk::Align::Start);
    widget.add_css_class("title-3");
    widget
}

fn code() -> gtk::Label {
    let widget = label(gtk::Align::Start);
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
    widget.set_size_request(36, 36);
    widget
}

fn spinner() -> gtk::Spinner {
    let widget = gtk::Spinner::new();
    widget.start();
    widget
}
