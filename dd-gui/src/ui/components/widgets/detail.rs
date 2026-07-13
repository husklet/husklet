#![allow(unused_imports, dead_code)]
use gtk::prelude::*;

pub(crate) fn detail_root() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 18);
    b.set_margin_top(22);
    b.set_margin_bottom(22);
    b.set_margin_start(24);
    b.set_margin_end(24);
    b
}

pub(crate) fn detail_header(title: &str, subtitle: &str, actions: Vec<gtk::Button>) -> gtk::Widget {
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_hexpand(true);
    titles.set_valign(gtk::Align::Center);
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.add_css_class("title-2");
    let s = gtk::Label::new(Some(subtitle));
    s.set_xalign(0.0);
    s.set_wrap(true);
    s.add_css_class("dim-label");
    titles.append(&t);
    titles.append(&s);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&titles);
    for b in actions {
        b.set_valign(gtk::Align::Center);
        row.append(&b);
    }
    row.upcast()
}

/// A titled section: a caption header and either its value rows or a dim em-dash.
pub(crate) fn section(title: &str, lines: &[String]) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let cap = gtk::Label::new(Some(&title.to_uppercase()));
    cap.set_xalign(0.0);
    cap.add_css_class("dd-section-title");
    b.append(&cap);

    if lines.is_empty() {
        let l = gtk::Label::new(Some("—"));
        l.set_xalign(0.0);
        l.add_css_class("dim-label");
        b.append(&l);
    } else {
        for line in lines {
            let l = gtk::Label::new(Some(line));
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_selectable(true);
            b.append(&l);
        }
    }
    b.upcast()
}
