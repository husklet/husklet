#![allow(unused_imports, dead_code)]
use crate::{AppModel, Msg};
use gtk::prelude::*;
use relm4::ComponentSender;

/// On first launch, offer to point the `docker` CLI at our daemon (the `dd` context). A small,
/// frameless dialog using the app's own pill buttons.
pub fn prompt_switch_context(
    parent: &gtk::ApplicationWindow,
    sender: &ComponentSender<AppModel>,
    current: &str,
) {
    let title = gtk::Label::new(Some("Use dd as your Docker context?"));
    title.set_xalign(0.0);
    title.add_css_class("title-3");
    let detail = gtk::Label::new(Some(&format!("Point the docker CLI at this app (switch from \u{201c}{current}\u{201d} to \u{201c}dd\u{201d}).")));
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.set_max_width_chars(34);
    detail.add_css_class("dim-label");

    let cancel = gtk::Button::with_label("Not now");
    cancel.add_css_class("dd-btn");
    let ok = gtk::Button::with_label("Switch to dd");
    ok.add_css_class("dd-btn");
    ok.add_css_class("suggested-action");
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
        s.input(Msg::SetContext("dd".to_string()));
        w2.close();
    });
    win.present();
}
