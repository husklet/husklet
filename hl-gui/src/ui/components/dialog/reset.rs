#![allow(unused_imports, dead_code)]
use crate::{AppModel, Msg};
use gtk::prelude::*;
use relm4::ComponentSender;

/// Confirm a full reset (remove all containers/volumes/networks). Frameless dialog, pill buttons.
pub fn confirm_reset(parent: &gtk::ApplicationWindow, sender: &ComponentSender<AppModel>) {
    let title = gtk::Label::new(Some("Reset dd?"));
    title.set_xalign(0.0);
    title.add_css_class("dd-onboard-head");
    let detail = gtk::Label::new(Some(
        "This removes all containers, volumes and networks. Your images are kept.",
    ));
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.set_max_width_chars(36);
    detail.add_css_class("dim-label");

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("dd-btn");
    let ok = gtk::Button::with_label("Reset");
    ok.add_css_class("dd-btn");
    ok.add_css_class("dd-danger");
    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btns.set_halign(gtk::Align::End);
    btns.set_margin_top(8);
    btns.append(&cancel);
    btns.append(&ok);

    let v = gtk::Box::new(gtk::Orientation::Vertical, 8);
    v.add_css_class("dd-dialog");
    v.set_margin_top(20);
    v.set_margin_bottom(18);
    v.set_margin_start(22);
    v.set_margin_end(22);
    v.append(&title);
    v.append(&detail);
    v.append(&btns);

    let win = gtk::Window::builder()
        .modal(true)
        .resizable(false)
        .decorated(false)
        .child(&v)
        .build();
    win.set_transient_for(Some(parent));
    win.add_css_class("dd-modal");
    let w1 = win.clone();
    cancel.connect_clicked(move |_| w1.close());
    let s = sender.clone();
    let w2 = win.clone();
    ok.connect_clicked(move |_| {
        s.input(Msg::Reset);
        w2.close();
    });
    win.present();
}
