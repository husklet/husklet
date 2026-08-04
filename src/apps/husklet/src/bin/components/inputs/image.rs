const CURATED_IMAGES: &[ImageTemplate] = &[
    ImageTemplate {
        name: "Ubuntu 24.04 LTS",
        reference: "ubuntu:24.04",
        description: "Latest Ubuntu LTS — the default dev base.",
    },
    ImageTemplate {
        name: "Ubuntu 22.04 LTS",
        reference: "ubuntu:22.04",
        description: "Previous Ubuntu LTS.",
    },
    ImageTemplate {
        name: "Debian 12 (Bookworm)",
        reference: "debian:bookworm",
        description: "Stable Debian.",
    },
    ImageTemplate {
        name: "Alpine",
        reference: "alpine:latest",
        description: "Tiny musl-based image.",
    },
    ImageTemplate {
        name: "Fedora",
        reference: "fedora:latest",
        description: "Fedora — recent packages.",
    },
    ImageTemplate {
        name: "AlmaLinux 9",
        reference: "almalinux:9",
        description: "RHEL-compatible enterprise base.",
    },
];

pub(crate) struct ImagePicker {
    architecture: Arch,
    templates: &'static [ImageTemplate],
}

impl ImagePicker {
    pub(crate) fn new(architecture: Arch) -> Self {
        Self {
            architecture,
            templates: CURATED_IMAGES,
        }
    }

    /// The image-selection window: a list of predefined templates for the workspace's currently-selected
    /// architecture. Clicking a row fills the IMAGE field. (Custom refs can still be typed directly.)
    pub(crate) fn present<F>(&self, parent: &gtk::Window, selected: F)
    where
        F: Fn(&str) + Clone + 'static,
    {
        let win = gtk::Window::builder()
            .title("Choose an image")
            .modal(true)
            .transient_for(parent)
            .default_width(480)
            .default_height(440)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("imgpick");

        let head = gtk::Box::new(gtk::Orientation::Vertical, 2);
        head.add_css_class("imghead");
        let t = gtk::Label::new(Some("Predefined images"));
        t.add_css_class("ptitle");
        t.set_xalign(0.0);
        let sub = gtk::Label::new(Some(&format!(
            "for {} — or Cancel and type a custom image reference",
            self.architecture.as_str()
        )));
        sub.add_css_class("fhint");
        sub.set_xalign(0.0);
        head.append(&t);
        head.append(&sub);
        root.append(&head);

        let list = gtk::ListBox::new();
        list.add_css_class("imglist");
        list.set_selection_mode(gtk::SelectionMode::None);
        for template in self.templates {
            let row = gtk::ListBoxRow::new();
            let bx = gtk::Box::new(gtk::Orientation::Vertical, 2);
            bx.add_css_class("imgrow");
            let n = gtk::Label::new(Some(template.name));
            n.add_css_class("imgname");
            n.set_xalign(0.0);
            let r = gtk::Label::new(Some(&format!("{}  ·  {}", template.reference, template.description)));
            r.add_css_class("imgref");
            r.set_xalign(0.0);
            bx.append(&n);
            bx.append(&r);
            row.set_child(Some(&bx));
            let click = gtk::GestureClick::new();
            let selected = selected.clone();
            let win2 = win.clone();
            let reference = template.reference;
            click.connect_released(move |_, _, _, _| {
                selected(reference);
                win2.close();
            });
            row.add_controller(click);
            list.append(&row);
        }
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).child(&list).build();
        root.append(&scroller);

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer.add_css_class("footer");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        footer.append(&spacer);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("btn");
        let win3 = win.clone();
        cancel.connect_clicked(move |_| win3.close());
        footer.append(&cancel);
        root.append(&footer);

        win.set_child(Some(&root));
        let keys = gtk::EventControllerKey::new();
        let dismiss = win.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                dismiss.close();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        win.add_controller(keys);
        win.present();
        host::appearance::Appearance::apply();
    }
}
use crate::*;

struct ImageTemplate {
    name: &'static str,
    reference: &'static str,
    description: &'static str,
}
