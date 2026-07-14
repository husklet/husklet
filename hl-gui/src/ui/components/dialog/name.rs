#![allow(unused_imports, dead_code)]
use crate::{AppModel, Msg};
use gtk::prelude::*;
use relm4::ComponentSender;

/// A small modal name-entry dialog (used to create networks/volumes). On Create it sends `make(name)`.
pub fn prompt_name(
    parent: &gtk::ApplicationWindow,
    title: &str,
    placeholder: &str,
    sender: &ComponentSender<AppModel>,
    make: fn(String) -> Msg,
) {
    let v = gtk::Box::new(gtk::Orientation::Vertical, 12);
    v.set_margin_top(18);
    v.set_margin_bottom(18);
    v.set_margin_start(20);
    v.set_margin_end(20);
    v.add_css_class("hl-dialog");

    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.add_css_class("hl-onboard-head");

    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_activates_default(true);
    entry.set_width_request(240);

    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btns.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("hl-btn");
    let ok = gtk::Button::with_label("Create");
    ok.add_css_class("hl-btn");
    ok.add_css_class("suggested-action");
    btns.append(&cancel);
    btns.append(&ok);

    v.append(&t);
    v.append(&entry);
    v.append(&btns);

    let win = gtk::Window::builder()
        .modal(true)
        .resizable(false)
        .decorated(false)
        .child(&v)
        .build();
    win.set_transient_for(Some(parent));
    win.add_css_class("hl-modal");

    let w1 = win.clone();
    cancel.connect_clicked(move |_| w1.close());
    let s = sender.clone();
    let w2 = win.clone();
    let e = entry.clone();
    ok.connect_clicked(move |_| {
        let name = e.text().as_str().trim().to_string();
        if !name.is_empty() {
            s.input(make(name));
        }
        w2.close();
    });
    win.present();
}
