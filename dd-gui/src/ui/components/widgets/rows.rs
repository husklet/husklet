#![allow(unused_imports, dead_code)]
use crate::{AppModel, Msg};
use gtk::prelude::*;
use relm4::ComponentSender;

/// A "title + description on the left, action button on the right" row for the Get Started card.
pub(crate) fn action_row(
    title: &str,
    desc: &str,
    btn_label: &str,
    primary: bool,
    sender: &ComponentSender<AppModel>,
    msg: impl Fn() -> Msg + 'static,
) -> gtk::Box {
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.add_css_class("heading");
    let d = gtk::Label::new(Some(desc));
    d.set_xalign(0.0);
    d.set_wrap(true);
    d.add_css_class("dim-label");
    d.add_css_class("caption");
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 1);
    texts.set_hexpand(true);
    texts.set_valign(gtk::Align::Center);
    texts.append(&t);
    texts.append(&d);
    let btn = gtk::Button::with_label(btn_label);
    btn.add_css_class("dd-btn");
    if primary {
        btn.add_css_class("suggested-action");
    }
    btn.set_valign(gtk::Align::Center);
    {
        let s = sender.clone();
        btn.connect_clicked(move |_| s.input(msg()));
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(&texts);
    row.append(&btn);
    row
}

pub(crate) fn nav_item(title: &str, subtitle: &str, running: bool) -> gtk::ListBoxRow {
    // No ad-hoc margins — the shared `.navigation-sidebar > row` padding governs both sidebars.
    let v = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    t.add_css_class("dd-listrow-title"); // same title weight as the containers list
    v.append(&t);
    if !subtitle.is_empty() {
        let s = gtk::Label::new(Some(subtitle));
        s.set_xalign(0.0);
        s.add_css_class("dd-listrow-sub");
        if running {
            s.add_css_class("success");
        }
        v.append(&s);
    }
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&v));
    row
}

pub(crate) fn dim_row(text: &str) -> gtk::ListBoxRow {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_margin_top(6);
    l.set_margin_bottom(6);
    l.set_margin_start(8);
    l.add_css_class("dim-label");
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.set_child(Some(&l));
    row
}

pub(crate) fn text_btn(
    label: &str,
    css: &str,
    sender: &ComponentSender<AppModel>,
    msg: impl Fn() -> Msg + 'static,
) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("dd-btn");
    if !css.is_empty() {
        b.add_css_class(css);
    }
    let s = sender.clone();
    b.connect_clicked(move |_| s.input(msg()));
    b
}

/// A frameless "＋ New …" action row at the top of a resource list (sends its Msg on click).
pub(crate) fn new_row(
    label: &str,
    sender: &ComponentSender<AppModel>,
    make: impl Fn() -> Msg + 'static,
) -> gtk::ListBoxRow {
    let b = gtk::Button::with_label(label);
    b.set_has_frame(false);
    b.set_halign(gtk::Align::Start);
    b.add_css_class("dd-popitem");
    let s = sender.clone();
    b.connect_clicked(move |_| s.input(make()));
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.set_child(Some(&b));
    row
}
