use gtk::prelude::*;

pub const TITLE: &str = "Workspaces";

/// Toolkit composition for the home screen. Product behavior is wired by Husklet.
pub struct View {
    pub widget: gtk::Box,
    pub workspaces: gtk::ListBox,
    pub create: gtk::Button,
}

impl View {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("strip");
        let title = gtk::Label::new(Some(TITLE));
        title.add_css_class("h");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        header.append(&title);

        let create = gtk::Button::with_label("+ New");
        create.add_css_class("btn");
        create.add_css_class("primary");
        create.set_valign(gtk::Align::Center);
        header.append(&create);
        widget.append(&header);

        let workspaces = gtk::ListBox::new();
        workspaces.add_css_class("wslist");
        workspaces.set_selection_mode(gtk::SelectionMode::None);
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&workspaces)
            .build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        widget.append(&scroller);

        Self {
            widget,
            workspaces,
            create,
        }
    }
}
