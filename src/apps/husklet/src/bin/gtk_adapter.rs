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
            .build();
        if let Some(parent) = parent {
            window.set_transient_for(Some(parent));
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_top(20);
        content.set_margin_bottom(20);
        content.set_margin_start(20);
        content.set_margin_end(20);

        let title = gtk::Label::new(Some(&model.title));
        title.set_xalign(0.0);
        title.add_css_class("title-3");
        content.append(&title);
        if let Some(detail) = model.detail {
            let detail = gtk::Label::new(Some(&detail));
            detail.set_xalign(0.0);
            detail.set_wrap(true);
            content.append(&detail);
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
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    on_selected(path);
                }
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
    label: gtk::Label,
    swatch: gtk::DrawingArea,
}

impl ColorPicker {
    pub fn new(value: &str) -> Self {
        let color = Rc::new(RefCell::new(gtk::gdk::RGBA::BLACK));
        let label = gtk::Label::new(None);
        let swatch = gtk::DrawingArea::new();
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
            let label = label.clone();
            let swatch = swatch.clone();
            button.connect_clicked(move |button| {
                if active.replace(true) {
                    return;
                }
                let parent = button.root().and_downcast::<gtk::Window>();
                let initial = *color.borrow();
                let active = active.clone();
                let color = color.clone();
                let label = label.clone();
                let swatch = swatch.clone();
                dialog.choose_rgba(
                    parent.as_ref(),
                    Some(&initial),
                    gtk::gio::Cancellable::NONE,
                    move |result| {
                        if let Ok(selected) = result {
                            *color.borrow_mut() = selected;
                            label.set_text(&Self::format(&selected));
                            swatch.queue_draw();
                        }
                        active.set(false);
                    },
                );
            });
        }

        let picker = Self {
            button,
            color,
            label,
            swatch,
        };
        picker.set_value(value);
        picker
    }

    pub fn widget(&self) -> &gtk::Button {
        &self.button
    }

    pub fn set_value(&self, value: &str) {
        if let Ok(color) = gtk::gdk::RGBA::parse(value) {
            *self.color.borrow_mut() = color;
            self.label.set_text(&Self::format(&color));
            self.swatch.queue_draw();
        }
    }

    pub fn value(&self) -> String {
        Self::format(&self.color.borrow())
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

#[derive(Clone)]
pub struct FontPicker(gtk::FontDialogButton);

impl FontPicker {
    pub fn new(family: &str) -> Self {
        let button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
        button.set_use_font(true);
        button.set_use_size(false);
        let picker = Self(button);
        picker.set_value(family);
        picker
    }

    pub fn widget(&self) -> &gtk::FontDialogButton {
        &self.0
    }

    pub fn set_value(&self, family: &str) {
        self.0.set_font_desc(&gtk::pango::FontDescription::from_string(family));
    }

    pub fn value(&self) -> String {
        self.0
            .font_desc()
            .and_then(|description| description.family().map(|family| family.to_string()))
            .unwrap_or_default()
    }
}
