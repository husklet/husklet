//! A compact choice whose closed width is independent of its options.

use std::cell::{Cell, OnceCell};
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

const NONE: i64 = -1;

#[derive(Default)]
pub(crate) struct State {
    selected: Cell<i64>,
    label: OnceCell<gtk::Label>,
    options: OnceCell<gtk::Box>,
}

#[glib::object_subclass]
impl ObjectSubclass for State {
    const NAME: &'static str = "HlChoice";
    type Type = Choice;
    type ParentType = gtk::ToggleButton;
}

impl ObjectImpl for State {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
        PROPERTIES.get_or_init(|| {
            vec![
                glib::ParamSpecInt64::builder("selected")
                    .minimum(NONE)
                    .maximum(i64::from(u32::MAX))
                    .default_value(NONE)
                    .read_only()
                    .build(),
            ]
        })
    }

    fn property(&self, _id: usize, spec: &glib::ParamSpec) -> glib::Value {
        match spec.name() {
            "selected" => self.selected.get().to_value(),
            name => unreachable!("unknown choice property {name}"),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();
        self.selected.set(NONE);
        let obj = self.obj();
        obj.add_css_class("choice");
        obj.set_accessible_role(gtk::AccessibleRole::ComboBox);
        obj.set_hexpand(true);
        obj.set_halign(gtk::Align::Fill);

        let closed = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let label = gtk::Label::new(Some("Choose…"));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        closed.append(&label);
        closed.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&closed));
        let options = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&options));
        overlay.add_overlay(&popover);
        obj.set_child(Some(&overlay));
        self.label.set(label).expect("choice label constructed once");
        self.options.set(options).expect("choice options constructed once");

        let weak_popover = popover.downgrade();
        obj.connect_toggled(move |button| {
            let Some(popover) = weak_popover.upgrade() else { return };
            if button.is_active() && button.root().is_some() {
                popover.popup()
            } else {
                popover.popdown()
            }
        });
        let weak_obj = obj.downgrade();
        popover.connect_closed(move |_| {
            if let Some(obj) = weak_obj.upgrade() {
                obj.set_active(false)
            }
        });
    }
}

impl WidgetImpl for State {}
impl ButtonImpl for State {}
impl ToggleButtonImpl for State {}

glib::wrapper! {
    pub(crate) struct Choice(ObjectSubclass<State>)
        @extends gtk::ToggleButton, gtk::Button, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl Choice {
    pub(crate) fn set_width_chars(&self, count: i32) {
        self.imp()
            .label
            .get()
            .expect("constructed choice")
            .set_width_chars(count);
    }

    fn selected(&self) -> Option<u32> {
        u32::try_from(self.imp().selected.get()).ok()
    }

    fn set_selected(&self, selected: Option<u32>) {
        let value = selected.map_or(NONE, i64::from);
        if self.imp().selected.replace(value) != value {
            self.notify("selected")
        }
        let label = selected
            .and_then(|index| self.option(index))
            .and_then(|button| button.label())
            .unwrap_or_else(|| "Choose…".into());
        self.imp().label.get().expect("constructed choice").set_label(&label);
    }

    fn option(&self, index: u32) -> Option<gtk::Button> {
        self.imp()
            .options
            .get()?
            .first_child()
            .and_then(|first| (0..index).try_fold(first, |child, _| child.next_sibling()))?
            .downcast::<gtk::Button>()
            .ok()
    }
}

pub(crate) fn widget() -> Choice {
    glib::Object::new()
}

fn choice(widget: &gtk::Widget) -> Option<&Choice> {
    widget.downcast_ref::<Choice>()
}

pub(crate) fn selected(widget: &gtk::Widget) -> Option<u32> {
    choice(widget)?.selected()
}

pub(crate) fn connect_selected(widget: &gtk::Widget, callback: impl Fn() + 'static) -> bool {
    let Some(choice) = choice(widget) else { return false };
    choice.connect_notify_local(Some("selected"), move |_, _| callback());
    true
}

pub(crate) fn set_selected(widget: &gtk::Widget, selected: Option<u32>) {
    if let Some(choice) = choice(widget) {
        choice.set_selected(selected)
    }
}

pub(crate) fn set_options(widget: &gtk::Widget, labels: &[&str]) -> bool {
    let Some(choice) = choice(widget) else { return false };
    let options = choice.imp().options.get().expect("constructed choice");
    while let Some(child) = options.last_child() {
        options.remove(&child)
    }
    for (index, label) in labels.iter().enumerate() {
        let button = gtk::Button::with_label(label);
        button.add_css_class("flat");
        button.set_halign(gtk::Align::Fill);
        let weak_choice = choice.downgrade();
        button.connect_clicked(move |_| {
            let Some(choice) = weak_choice.upgrade() else { return };
            choice.set_selected(u32::try_from(index).ok());
            choice.set_active(false);
        });
        options.append(&button);
    }
    true
}
