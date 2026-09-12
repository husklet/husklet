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
    allocating: Cell<bool>,
    setting_position: Cell<bool>,
    expanded: Cell<Option<bool>>,
    layout: OnceCell<gtk::Box>,
    paned: OnceCell<gtk::Paned>,
}

impl Default for Pane {
    fn default() -> Self {
        Self {
            breakpoint: Cell::new(640),
            wide_position: Cell::new(160),
            allocating: Cell::new(false),
            setting_position: Cell::new(false),
            expanded: Cell::new(None),
            layout: OnceCell::new(),
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
            vec![
                glib::ParamSpecInt::builder("breakpoint")
                    .minimum(240)
                    .maximum(4096)
                    .default_value(640)
                    .read_only()
                    .build(),
            ]
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
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.add_css_class("hl-responsive-divider");
        paned.set_accessible_role(gtk::AccessibleRole::Separator);
        paned.update_property(&[
            gtk::accessible::Property::Label("Resize navigation"),
            gtk::accessible::Property::Description("Drag or use the keyboard to resize the navigation pane"),
            gtk::accessible::Property::Orientation(gtk::Orientation::Vertical),
            gtk::accessible::Property::ValueMin(0.0),
        ]);
        let responsive = self.obj().downgrade();
        paned.connect_position_notify(move |paned| {
            let value = paned.position();
            let text = format!("{value} pixels");
            paned.update_property(&[
                gtk::accessible::Property::ValueNow(f64::from(value)),
                gtk::accessible::Property::ValueText(&text),
            ]);
            if let Some(responsive) = responsive.upgrade() {
                remember_position(responsive.upcast_ref(), value);
            }
        });
        paned.connect_orientation_notify(|paned| {
            let separator = match paned.orientation() {
                gtk::Orientation::Horizontal => gtk::Orientation::Vertical,
                gtk::Orientation::Vertical => gtk::Orientation::Horizontal,
                _ => unreachable!("GTK orientations are closed"),
            };
            paned.update_property(&[gtk::accessible::Property::Orientation(separator)]);
        });
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(false);
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(true);
        // The paned is the entire wide branch, not a naturally-sized row in
        // the private vertical box. Without vertical expansion GTK stops the
        // rendered surface at the content's minimum height, which leaves the
        // rest of a desktop pane blank and needlessly scrolls its navigation.
        paned.set_vexpand(true);
        paned.set_visible(false);
        layout.append(&paned);
        layout.set_parent(&*self.obj());
        self.layout.set(layout).expect("responsive layout constructed once");
        self.paned.set(paned).expect("responsive pane constructed once");
    }

    fn dispose(&self) {
        if let Some(layout) = self.layout.get() {
            layout.unparent();
        }
    }
}

impl WidgetImpl for Pane {
    fn request_mode(&self) -> gtk::SizeRequestMode {
        self.layout().request_mode()
    }

    fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
        self.layout().measure(orientation, for_size)
    }

    fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
        self.allocating.set(true);
        let layout = self.layout();
        let paned = self.paned();
        let expanded = width >= self.breakpoint.get();
        let branch_changed = self.expanded.replace(Some(expanded)) != Some(expanded);
        let wide_position = self.wide_position.get();
        if let Some(compact) = layout.first_child().filter(|child| !child.eq(paned)) {
            compact.set_visible(!expanded);
        }
        if expanded {
            if let Some(body) = layout.last_child().filter(|child| !child.eq(paned)) {
                layout.remove(&body);
                paned.set_end_child(Some(&body));
            }
            paned.set_visible(true);
            paned.set_position(wide_position);
        } else if let Some(body) = paned.end_child() {
            paned.set_end_child(gtk::Widget::NONE);
            layout.append(&body);
            paned.set_visible(false);
        }
        // Reparenting invalidates GTK's prior measurement cache. Re-measure
        // the chosen branch before allocating it so narrow/wide transitions
        // never rely on stale geometry.
        layout.measure(gtk::Orientation::Horizontal, -1);
        layout.measure(gtk::Orientation::Vertical, width);
        layout.allocate(width, height, baseline, None);
        if branch_changed {
            // The first allocation realizes the reparented branch; the second
            // includes every newly mapped descendant in this same frame.
            layout.measure(gtk::Orientation::Horizontal, -1);
            layout.measure(gtk::Orientation::Vertical, width);
            layout.allocate(width, height, baseline, None);
        }
        paned.update_property(&[gtk::accessible::Property::ValueMax(f64::from(width))]);
        self.allocating.set(false);
    }
}

impl Pane {
    fn layout(&self) -> &gtk::Box {
        self.layout.get().expect("responsive layout is constructed")
    }

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

/// Places the compact navigation, wide navigation, and single shared body in
/// their stable slots. Only the two lightweight navigation branches differ;
/// the body is reparented at allocation time and is never duplicated.
pub(crate) fn attach(widget: &gtk::Widget, child: &gtk::Widget, index: usize) -> bool {
    let Some(pane) = widget.downcast_ref::<ResponsivePane>() else {
        return false;
    };
    let imp = pane.imp();
    match index {
        0 => {
            // Compact navigation is the header for the shared body. Give it
            // the complete cross-axis allocation; otherwise GTK centers the
            // child's natural width and long page names are clipped even when
            // their controls explicitly request `width=fill`.
            child.set_hexpand(true);
            child.set_halign(gtk::Align::Fill);
            imp.layout().prepend(child);
        }
        1 => imp.paned().set_start_child(Some(child)),
        2 => imp.paned().set_end_child(Some(child)),
        _ => return false,
    }
    true
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
    let imp = pane.imp();
    imp.wide_position.set(position);
    imp.setting_position.set(true);
    imp.paned().set_position(position);
    imp.setting_position.set(false);
    true
}

/// Whether a native position notification represents a person's divider gesture.
///
/// Applying the producer's controlled `position` prop and restoring it during an
/// allocation both notify GTK. Reporting either one back as input makes an
/// initial render indistinguishable from a drag and can overwrite a stored pane
/// width before it has loaded.
pub(crate) fn reports_position_change(widget: &gtk::Widget) -> bool {
    widget.downcast_ref::<ResponsivePane>().is_some_and(|pane| {
        let imp = pane.imp();
        !imp.allocating.get() && !imp.setting_position.get()
    })
}

pub(crate) fn remember_position(widget: &gtk::Widget, position: i32) {
    if let Some(pane) = widget.downcast_ref::<ResponsivePane>() {
        if !pane.imp().allocating.get()
            && pane.imp().paned().is_visible()
            && pane.imp().paned().start_child().is_some_and(|child| child.is_visible())
        {
            pane.imp().wide_position.set(position);
        }
    }
}
