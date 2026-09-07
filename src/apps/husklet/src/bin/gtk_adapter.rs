//! GTK adapters for generic input components.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use hl_gui::{Action, Dialog as DialogModel, EventId, Role};

/// Non-blocking GTK presentation of a toolkit-neutral dialog model.
pub struct Dialog;

impl Dialog {
    pub fn present(
        parent: Option<&gtk::Window>,
        model: DialogModel,
        on_action: impl Fn(EventId) + 'static,
    ) -> gtk::Window {
        let window = gtk::Window::builder()
            .title(&model.title)
            .modal(true)
            .resizable(false)
            .default_width(420)
            .accessible_role(gtk::AccessibleRole::Dialog)
            .build();
        if let Some(parent) = parent {
            window.set_transient_for(Some(parent));
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_top(20);
        content.set_margin_bottom(20);
        content.set_margin_start(20);
        content.set_margin_end(20);

        // Accessibility, not decoration: a dialog that asks the user to decide something about their
        // own work is the worst possible place to be unreachable. The title names the window and the
        // detail -- which is where an engine refusal reason arrives verbatim -- is announced with it
        // rather than left as loose text a screen reader has no reason to visit.
        let title = gtk::Label::new(Some(&model.title));
        title.set_xalign(0.0);
        title.add_css_class("title-3");
        title.set_accessible_role(gtk::AccessibleRole::Heading);
        content.append(&title);
        window.update_relation(&[gtk::accessible::Relation::LabelledBy(&[title.upcast_ref()])]);
        if let Some(detail) = model.detail {
            let label = gtk::Label::new(Some(&detail));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_accessible_role(gtk::AccessibleRole::Label);
            label.update_property(&[gtk::accessible::Property::Label(&detail)]);
            content.append(&label);
            window.update_relation(&[gtk::accessible::Relation::DescribedBy(&[label.upcast_ref()])]);
        }

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let handler = Rc::new(on_action);
        for action in model.actions {
            let role = action.role;
            let button = Self::button(&window, action, handler.clone());
            if role == Role::Suggested {
                window.set_default_widget(Some(&button));
            }
            actions.append(&button);
        }
        content.append(&actions);
        window.set_child(Some(&content));

        let keys = gtk::EventControllerKey::new();
        let dismiss = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                dismiss.close();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        window.add_controller(keys);
        window.present();
        window
    }

    fn button(window: &gtk::Window, action: Action, handler: Rc<impl Fn(EventId) + 'static>) -> gtk::Button {
        let button = gtk::Button::with_label(&action.label);
        // A button whose only name is its rendered glyphs has no name at all to anything that does
        // not read pixels. Name it explicitly rather than relying on the label child being walked.
        button.update_property(&[gtk::accessible::Property::Label(&action.label)]);
        match action.role {
            Role::Suggested => button.add_css_class("suggested-action"),
            Role::Destructive => button.add_css_class("destructive-action"),
            Role::Default => {}
        }
        let window = window.clone();
        button.connect_clicked(move |_| {
            handler(action.id.clone());
            window.close();
        });
        button
    }
}

/// Asynchronous native directory selection through GTK's platform backend.
pub struct DirectoryPicker {
    title: String,
}

impl DirectoryPicker {
    pub fn new(title: impl Into<String>) -> Self {
        Self { title: title.into() }
    }

    pub fn present(self, parent: Option<&gtk::Window>, on_selected: impl Fn(std::path::PathBuf) + 'static) {
        let dialog = gtk::FileDialog::builder().title(self.title).modal(true).build();
        dialog.select_folder(parent, gtk::gio::Cancellable::NONE, move |result| {
            if let Some(path) = result.ok().and_then(|file| file.path()) {
                on_selected(path);
            }
        });
    }
}

/// Opens a URI through the host desktop registered with GIO.
pub struct Uri(String);

impl Uri {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn open(&self) -> Result<(), gtk::glib::Error> {
        gtk::gio::AppInfo::launch_default_for_uri(&self.0, gtk::gio::AppLaunchContext::NONE)
    }
}

#[derive(Clone)]
pub struct ColorPicker {
    button: gtk::Button,
    color: Rc<RefCell<gtk::gdk::RGBA>>,
    stored: Rc<RefCell<String>>,
    label: gtk::Label,
    swatch: gtk::DrawingArea,
    changed: Rc<RefCell<Vec<Rc<dyn Fn(&str)>>>>,
}

enum ColorValue {
    Valid(gtk::gdk::RGBA, String),
    Invalid(String),
}

impl ColorValue {
    fn parse(value: &str) -> Self {
        gtk::gdk::RGBA::parse(value).map_or_else(
            |_| Self::Invalid(value.to_owned()),
            |color| Self::Valid(color, ColorPicker::format(&color)),
        )
    }

    fn stored(&self) -> &str {
        match self {
            Self::Valid(_, value) | Self::Invalid(value) => value,
        }
    }
}

impl ColorPicker {
    pub fn new(value: &str) -> Self {
        let color = Rc::new(RefCell::new(gtk::gdk::RGBA::BLACK));
        let stored = Rc::new(RefCell::new(value.to_owned()));
        let label = gtk::Label::new(None);
        let swatch = gtk::DrawingArea::new();
        let changed: Rc<RefCell<Vec<Rc<dyn Fn(&str)>>>> = Rc::new(RefCell::new(Vec::new()));
        swatch.set_content_width(28);
        swatch.set_content_height(18);
        {
            let color = color.clone();
            swatch.set_draw_func(move |_, context, width, height| {
                let color = color.borrow();
                context.set_source_rgba(
                    color.red().into(),
                    color.green().into(),
                    color.blue().into(),
                    color.alpha().into(),
                );
                context.rectangle(0.5, 0.5, f64::from(width - 1), f64::from(height - 1));
                let _ = context.fill_preserve();
                context.set_source_rgba(0.5, 0.5, 0.5, 0.8);
                context.set_line_width(1.0);
                let _ = context.stroke();
            });
        }

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.append(&swatch);
        content.append(&label);
        let button = gtk::Button::new();
        button.set_child(Some(&content));

        let active = Rc::new(Cell::new(false));
        let dialog = gtk::ColorDialog::builder()
            .title("Pick a Color")
            .modal(true)
            .with_alpha(false)
            .build();
        {
            let active = active.clone();
            let color = color.clone();
            let stored = stored.clone();
            let label = label.clone();
            let swatch = swatch.clone();
            let changed = changed.clone();
            button.connect_clicked(move |button| {
                Self::choose(button, &dialog, &active, &color, &stored, &label, &swatch, &changed);
            });
        }

        let picker = Self {
            button,
            color,
            stored,
            label,
            swatch,
            changed,
        };
        picker.set_value(value);
        picker
    }

    fn choose(
        button: &gtk::Button,
        dialog: &gtk::ColorDialog,
        active: &Rc<Cell<bool>>,
        color: &Rc<RefCell<gtk::gdk::RGBA>>,
        stored: &Rc<RefCell<String>>,
        label: &gtk::Label,
        swatch: &gtk::DrawingArea,
        changed: &Rc<RefCell<Vec<Rc<dyn Fn(&str)>>>>,
    ) {
        if active.replace(true) {
            return;
        }
        let parent = button.root().and_downcast::<gtk::Window>();
        let initial = *color.borrow();
        let active = active.clone();
        let button = button.clone();
        let color = color.clone();
        let stored = stored.clone();
        let label = label.clone();
        let swatch = swatch.clone();
        let changed = changed.clone();
        dialog.choose_rgba(
            parent.as_ref(),
            Some(&initial),
            gtk::gio::Cancellable::NONE,
            move |result| {
                Self::apply(&result, &button, &active, &color, &stored, &label, &swatch, &changed);
            },
        );
    }

    fn apply(
        result: &Result<gtk::gdk::RGBA, gtk::glib::Error>,
        button: &gtk::Button,
        active: &Rc<Cell<bool>>,
        color: &Rc<RefCell<gtk::gdk::RGBA>>,
        stored: &Rc<RefCell<String>>,
        label: &gtk::Label,
        swatch: &gtk::DrawingArea,
        changed: &Rc<RefCell<Vec<Rc<dyn Fn(&str)>>>>,
    ) {
        if let Ok(selected) = result {
            let value = Self::format(selected);
            *color.borrow_mut() = *selected;
            stored.replace(value.clone());
            label.set_text(&value);
            swatch.set_visible(true);
            button.set_tooltip_text(None);
            swatch.queue_draw();
            for listener in changed.borrow().iter() {
                listener(&value);
            }
        }
        active.set(false);
    }

    pub fn widget(&self) -> &gtk::Button {
        &self.button
    }

    pub fn set_value(&self, value: &str) {
        let value = ColorValue::parse(value);
        self.stored.replace(value.stored().to_owned());
        match value {
            ColorValue::Valid(color, text) => {
                *self.color.borrow_mut() = color;
                self.label.set_text(&text);
                self.swatch.set_visible(true);
                self.button.set_tooltip_text(None);
                self.swatch.queue_draw();
            }
            ColorValue::Invalid(original) => {
                self.label.set_text("Invalid color");
                self.swatch.set_visible(false);
                self.button
                    .set_tooltip_text(Some(&format!("Invalid terminal color: {original}")));
            }
        }
        let value = self.value();
        for listener in self.changed.borrow().iter() {
            listener(&value);
        }
    }

    pub fn value(&self) -> String {
        self.stored.borrow().clone()
    }

    fn format(color: &gtk::gdk::RGBA) -> String {
        format!(
            "#{:02x}{:02x}{:02x}",
            (color.red().clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.green().clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.blue().clamp(0.0, 1.0) * 255.0).round() as u8
        )
    }
}

#[cfg(test)]
mod color_picker_tests {
    use super::ColorValue;

    #[test]
    fn invalid_colors_are_preserved_until_the_user_replaces_them() {
        assert_eq!(ColorValue::parse("legacy-not-a-color").stored(), "legacy-not-a-color");
        assert_eq!(ColorValue::parse("#AABBCC").stored(), "#aabbcc");
    }
}

#[derive(Clone)]
pub struct FontPicker {
    button: gtk::Button,
    stored: Rc<RefCell<String>>,
    changed: Rc<RefCell<Vec<Rc<dyn Fn(&str)>>>>,
}

impl FontPicker {
    pub fn new(family: &str) -> Self {
        let button = gtk::Button::new();
        let stored = Rc::new(RefCell::new(family.to_owned()));
        let changed: Rc<RefCell<Vec<Rc<dyn Fn(&str)>>>> = Rc::new(RefCell::new(Vec::new()));
        let dialog = gtk::FontDialog::builder().title("Pick a Font").modal(true).build();
        {
            let stored = stored.clone();
            let changed = changed.clone();
            button.connect_clicked(move |button| {
                let parent = button.root().and_downcast::<gtk::Window>();
                let initial = gtk::pango::FontDescription::from_string(&stored.borrow());
                let button = button.clone();
                let stored = stored.clone();
                let changed = changed.clone();
                dialog.choose_font(
                    parent.as_ref(),
                    Some(&initial),
                    gtk::gio::Cancellable::NONE,
                    move |result| {
                        let Ok(description) = result else { return };
                        let Some(family) = description.family() else { return };
                        let family = family.to_string();
                        stored.replace(family.clone());
                        button.set_label(&family);
                        button.set_tooltip_text(Some(&format!("Terminal font: {family}")));
                        for listener in changed.borrow().iter() {
                            listener(&family);
                        }
                    },
                );
            });
        }
        let picker = Self {
            button,
            stored,
            changed,
        };
        picker.set_value(family);
        picker
    }

    pub fn widget(&self) -> &gtk::Button {
        &self.button
    }

    pub fn set_value(&self, family: &str) {
        self.stored.replace(family.to_owned());
        self.button.set_label(family);
        self.button.set_tooltip_text(Some(&format!("Terminal font: {family}")));
        for listener in self.changed.borrow().iter() {
            listener(family);
        }
    }

    pub fn value(&self) -> String {
        self.stored.borrow().clone()
    }
}

#[cfg(test)]
mod font_picker_tests {
    use super::FontPicker;
    use gtk::prelude::ButtonExt;

    #[test]
    fn unavailable_font_families_remain_visible_and_are_not_discarded() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let picker = FontPicker::new("Definitely Missing Husklet Font");
            assert_eq!(picker.value(), "Definitely Missing Husklet Font");
            assert_eq!(
                picker.widget().label().as_deref(),
                Some("Definitely Missing Husklet Font")
            );

            picker.set_value("Another Missing Font");
            assert_eq!(picker.value(), "Another Missing Font");
            assert_eq!(picker.widget().label().as_deref(), Some("Another Missing Font"));
        });
        if !ran {
            eprintln!("skipped: no display connection");
        }
    }
}
