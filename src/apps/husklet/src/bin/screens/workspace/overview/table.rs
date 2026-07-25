use super::*;

pub(super) struct Table {
    pub(super) widget: gtk::ScrolledWindow,
    body: gtk::Box,
}

impl Table {
    pub(super) fn new(headers: &[&str]) -> Self {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.add_css_class("dmain");
        outer.append(&Self::row(headers, "thead"));

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_hexpand(true);
        outer.append(&body);

        let widget = gtk::ScrolledWindow::builder()
            .child(&outer)
            .hexpand(true)
            .vexpand(true)
            .build();
        Self { widget, body }
    }

    pub(super) fn fill(&self, rows: &[Vec<String>], error: Option<&str>) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        if let Some(error) = error {
            let label = gtk::Label::new(Some(error));
            label.add_css_class("dhint");
            label.set_margin_top(16);
            self.body.append(&label);
            return;
        }
        if rows.is_empty() {
            let label = gtk::Label::new(Some("— none —"));
            label.add_css_class("dhint");
            label.set_margin_top(16);
            label.set_halign(gtk::Align::Start);
            self.body.append(&label);
            return;
        }
        for row in rows {
            let cells: Vec<&str> = row.iter().map(String::as_str).collect();
            self.body.append(&Self::row(&cells, "tbody"));
        }
    }

    fn row(cells: &[&str], css: &str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("trow");
        row.add_css_class(css);
        for (index, cell) in cells.iter().enumerate() {
            let label = gtk::Label::new(Some(cell));
            label.set_xalign(0.0);
            label.set_hexpand(index == 0);
            label.set_width_chars(if index == 0 { 24 } else { 16 });
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("tcell");
            row.append(&label);
        }
        row
    }
}
