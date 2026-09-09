//! A compact modern color button without `ColorDialogButton`'s private stack.

use std::cell::{OnceCell, RefCell};
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

pub(crate) struct State {
    color: RefCell<gtk::gdk::RGBA>,
    swatch: OnceCell<gtk::DrawingArea>,
    label: OnceCell<gtk::Label>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            color: RefCell::new(gtk::gdk::RGBA::BLACK),
            swatch: OnceCell::new(),
            label: OnceCell::new(),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for State {
    const NAME: &'static str = "HlColorChoice";
    type Type = ColorChoice;
    type ParentType = gtk::Button;
}

impl ObjectImpl for State {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
        PROPERTIES.get_or_init(|| vec![glib::ParamSpecString::builder("color").build()])
    }

    fn property(&self, _id: usize, spec: &glib::ParamSpec) -> glib::Value {
        match spec.name() {
            "color" => super::field::color_value(&self.color.borrow()).to_value(),
            name => unreachable!("unknown color property {name}"),
        }
    }

    fn set_property(&self, _id: usize, value: &glib::Value, spec: &glib::ParamSpec) {
        match spec.name() {
            "color" => {
                if let Ok(color) = gtk::gdk::RGBA::parse(value.get::<String>().unwrap_or_default()) {
                    self.obj().set_rgba(color);
                }
            }
            name => unreachable!("unknown color property {name}"),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let swatch = gtk::DrawingArea::new();
        swatch.set_content_width(18);
        swatch.set_content_height(18);
        let weak = obj.downgrade();
        swatch.set_draw_func(move |_, context, width, height| {
            let Some(obj) = weak.upgrade() else { return };
            let color = obj.imp().color.borrow();
            context.set_source_rgba(
                f64::from(color.red()),
                f64::from(color.green()),
                f64::from(color.blue()),
                f64::from(color.alpha()),
            );
            context.rectangle(0.0, 0.0, f64::from(width), f64::from(height));
            let _ = context.fill();
        });
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        row.append(&swatch);
        row.append(&label);
        obj.set_child(Some(&row));
        self.swatch.set(swatch).expect("color swatch constructed once");
        self.label.set(label).expect("color label constructed once");
        obj.set_rgba(gtk::gdk::RGBA::BLACK);

        obj.connect_clicked(|button| {
            let parent = button.root().and_downcast::<gtk::Window>();
            let initial = *button.imp().color.borrow();
            let weak = button.downgrade();
            gtk::ColorDialog::new().choose_rgba(
                parent.as_ref(),
                Some(&initial),
                None::<&gtk::gio::Cancellable>,
                move |result| {
                    if let (Some(button), Ok(color)) = (weak.upgrade(), result) {
                        button.set_rgba(color)
                    }
                },
            );
        });
    }
}

impl WidgetImpl for State {}
impl ButtonImpl for State {}

glib::wrapper! {
    pub(crate) struct ColorChoice(ObjectSubclass<State>)
        @extends gtk::Button, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl ColorChoice {
    pub(crate) fn rgba(&self) -> gtk::gdk::RGBA {
        *self.imp().color.borrow()
    }

    pub(crate) fn set_rgba(&self, color: gtk::gdk::RGBA) {
        if *self.imp().color.borrow() == color {
            return;
        }
        *self.imp().color.borrow_mut() = color;
        let text = super::field::color_value(&color);
        self.imp()
            .label
            .get()
            .expect("constructed color label")
            .set_label(&text);
        self.update_property(&[gtk::accessible::Property::ValueText(text.as_str())]);
        self.imp().swatch.get().expect("constructed swatch").queue_draw();
        self.notify("color");
    }
}

pub(crate) fn widget() -> ColorChoice {
    glib::Object::new()
}
