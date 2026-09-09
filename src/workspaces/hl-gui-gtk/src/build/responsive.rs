//! An allocation-driven splitter with one authoritative body tree.

use std::cell::{Cell, OnceCell};
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use hl_gui::PropValue;

pub(super) struct Pane {
    breakpoint: Cell<i32>,
    wide_position: Cell<i32>,
    paned: OnceCell<gtk::Paned>,
}

impl Default for Pane {
    fn default() -> Self {
        Self {
            breakpoint: Cell::new(640),
            wide_position: Cell::new(160),
            paned: OnceCell::new(),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for Pane {
    const NAME: &'static str = "HlResponsivePane";
    type Type = ResponsivePane;
    type ParentType = gtk::Widget;
}

impl ObjectImpl for Pane {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
        PROPERTIES.get_or_init(|| {
            vec![glib::ParamSpecInt::builder("breakpoint")
                .minimum(240)
                .maximum(4096)
                .default_value(640)
                .read_only()
                .build()]
        })
    }

    fn property(&self, _id: usize, spec: &glib::ParamSpec) -> glib::Value {
        match spec.name() {
            "breakpoint" => self.breakpoint.get().to_value(),
            name => unreachable!("unknown responsive property {name}"),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_parent(&*self.obj());
        self.paned.set(paned).expect("responsive pane constructed once");
    }

    fn dispose(&self) {
        if let Some(paned) = self.paned.get() {
            paned.unparent();
        }
        while let Some(child) = self.obj().first_child() {
            child.unparent();
        }
    }
}

impl WidgetImpl for Pane {
    fn request_mode(&self) -> gtk::SizeRequestMode {
        self.paned().request_mode()
    }

    fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
        self.paned().measure(orientation, for_size)
    }

    fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
        let paned = self.paned();
        let expanded = width >= self.breakpoint.get();
        paned.set_position(if expanded { self.wide_position.get() } else { 0 });
        if let Some(navigation) = paned.start_child() {
            let changed = navigation.is_visible() != expanded;
            navigation.set_visible(expanded);
            if changed {
                paned.queue_allocate();
            }
        }
        if expanded {
            if let Some(body) = self.obj().last_child().filter(|child| !child.eq(paned)) {
                body.unparent();
                paned.set_end_child(Some(&body));
                paned.measure(gtk::Orientation::Horizontal, -1);
                paned.measure(gtk::Orientation::Vertical, width);
            }
            paned.allocate(width, height, baseline, None);
        } else if let Some(body) = paned.end_child() {
            paned.set_end_child(gtk::Widget::NONE);
            if let Some(navigation) = paned.start_child() {
                navigation.set_visible(false);
            }
            body.set_parent(&*self.obj());
            body.allocate(width, height, baseline, None);
        } else if let Some(body) = self.obj().last_child().filter(|child| !child.eq(paned)) {
            body.allocate(width, height, baseline, None);
        }
    }
}

impl Pane {
    fn paned(&self) -> &gtk::Paned {
        self.paned.get().expect("responsive pane is constructed")
    }
}

glib::wrapper! {
    pub(super) struct ResponsivePane(ObjectSubclass<Pane>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub(super) fn widget() -> ResponsivePane {
    glib::Object::new()
}

pub(crate) fn paned(widget: &gtk::Widget) -> Option<gtk::Paned> {
    widget
        .downcast_ref::<ResponsivePane>()
        .map(|pane| pane.imp().paned().clone())
}

pub(crate) fn set(widget: &gtk::Widget, value: &PropValue) {
    let pixels = value.as_number().unwrap_or(640.0).clamp(240.0, 4096.0) as i32;
    if let Some(pane) = widget.downcast_ref::<ResponsivePane>() {
        if pane.imp().breakpoint.replace(pixels) != pixels {
            pane.notify("breakpoint");
        }
        pane.queue_resize();
    }
}

pub(crate) fn set_position(widget: &gtk::Widget, position: i32) -> bool {
    let Some(pane) = widget.downcast_ref::<ResponsivePane>() else {
        return false;
    };
    pane.imp().wide_position.set(position);
    pane.imp().paned().set_position(position);
    true
}

pub(crate) fn remember_position(widget: &gtk::Widget, position: i32) {
    if let Some(pane) = widget.downcast_ref::<ResponsivePane>() {
        if pane.imp().paned().start_child().is_some_and(|child| child.is_visible()) {
            pane.imp().wide_position.set(position);
        }
    }
}
