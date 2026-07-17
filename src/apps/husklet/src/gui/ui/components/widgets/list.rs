#![allow(unused_imports, dead_code)]
use gtk::prelude::*;

// ---- sidebar helpers -------------------------------------------------------

pub(crate) fn nav_list() -> gtk::ListBox {
    let l = gtk::ListBox::new();
    l.set_selection_mode(gtk::SelectionMode::Single);
    l.add_css_class("navigation-sidebar");
    l
}

pub(crate) fn placeholder(text: &str) -> gtk::Widget {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim-label");
    l.set_vexpand(true);
    l.set_hexpand(true);
    l.set_valign(gtk::Align::Center);
    l.set_halign(gtk::Align::Center);
    l.upcast()
}

pub(crate) fn select_named(list: &gtk::ListBox, name: &str) {
    let mut i = 0;
    while let Some(row) = list.row_at_index(i) {
        if row.widget_name().as_str() == name {
            list.select_row(Some(&row));
            return;
        }
        i += 1;
    }
}

pub(crate) fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

pub(crate) fn clear_box(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}
