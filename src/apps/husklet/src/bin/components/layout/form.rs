use gtk::prelude::*;

pub(crate) struct Panel(gtk::Box);

impl Panel {
    pub(crate) fn new(title: &str) -> Self {
        let p = gtk::Box::new(gtk::Orientation::Vertical, 14);
        p.add_css_class("pane");
        let t = gtk::Label::new(Some(title));
        t.add_css_class("ptitle");
        t.set_xalign(0.0);
        p.append(&t);
        Self(p)
    }

    pub(crate) fn into_widget(self) -> gtk::Box {
        self.0
    }
}

pub(crate) struct Field;

impl Field {
    const ENTRY_ACTIVATES_DEFAULT: bool = true;

    pub(crate) fn toggle(title: &str, description: &str, switch: &gtk::Switch) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("dockrow");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let title = gtk::Label::new(Some(title));
        title.add_css_class("tt");
        title.set_xalign(0.0);
        let description = gtk::Label::new(Some(description));
        description.add_css_class("td");
        description.set_xalign(0.0);
        description.set_wrap(true);
        description.set_max_width_chars(46);
        text.append(&title);
        text.append(&description);
        row.append(&text);
        switch.set_valign(gtk::Align::Center);
        row.append(switch);
        row
    }

    pub(crate) fn entry(placeholder: &str, mono: bool) -> gtk::Entry {
        let e = gtk::Entry::new();
        e.set_placeholder_text(Some(placeholder));
        e.set_activates_default(Self::ENTRY_ACTIVATES_DEFAULT);
        if mono {
            e.add_css_class("mono");
        }
        e
    }

    pub(crate) fn text(label: &str, e: &gtk::Entry, hint: Option<&str>) -> gtk::Box {
        let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let l = gtk::Label::new(Some(label));
        l.add_css_class("flabel");
        l.set_xalign(0.0);
        b.append(&l);
        b.append(e);
        if let Some(h) = hint {
            let hl = gtk::Label::new(Some(h));
            hl.add_css_class("fhint");
            hl.set_xalign(0.0);
            // Wrap + cap the natural width, else a long hint forces the whole window wide (GTK sizes a
            // non-wrapping label to its full single-line width).
            hl.set_wrap(true);
            hl.set_max_width_chars(46);
            b.append(&hl);
        }
        b
    }

    /// A labelled row wrapping an arbitrary control (used for the OS/Arch segmented controls).
    pub(crate) fn labeled(label: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
        let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let l = gtk::Label::new(Some(label));
        l.add_css_class("flabel");
        l.set_xalign(0.0);
        b.append(&l);
        b.append(w);
        b
    }

    pub(crate) fn spin(label: &str, s: &gtk::SpinButton) -> gtk::Box {
        let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let l = gtk::Label::new(Some(label));
        l.add_css_class("flabel");
        l.set_xalign(0.0);
        s.set_halign(gtk::Align::Start);
        b.append(&l);
        b.append(s);
        b
    }
}

#[cfg(test)]
mod tests {
    use super::Field;

    #[test]
    fn single_line_entries_activate_the_window_default() {
        assert!(Field::ENTRY_ACTIVATES_DEFAULT);
    }
}

// =================================================================================================
// Window 3 — per-workspace Terminal window (native titlebar; full-width tabs below)
// =================================================================================================
