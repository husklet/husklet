//! GTK adapters for generic input components.

use gtk::prelude::*;
use std::rc::Rc;

use crate::{Action, Dialog as DialogModel, EventId, Role};

/// Non-blocking GTK presentation of a toolkit-neutral dialog model.
pub struct Dialog;

impl Dialog {
    pub fn present(
        parent: Option<&gtk::Window>,
        model: DialogModel,
        on_action: impl Fn(EventId) + 'static,
    ) {
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
            actions.append(&Self::button(&window, action, handler.clone()));
        }
        content.append(&actions);
        window.set_child(Some(&content));
        window.present();
    }

    fn button(
        window: &gtk::Window,
        action: Action,
        handler: Rc<impl Fn(EventId) + 'static>,
    ) -> gtk::Button {
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
        Self {
            title: title.into(),
        }
    }

    pub fn present(
        self,
        parent: Option<&gtk::Window>,
        on_selected: impl Fn(std::path::PathBuf) + 'static,
    ) {
        let dialog = gtk::FileDialog::builder()
            .title(self.title)
            .modal(true)
            .build();
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
pub struct ColorPicker(gtk::ColorDialogButton);

impl ColorPicker {
    pub fn new(value: &str) -> Self {
        let picker = Self(gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new())));
        picker.set_value(value);
        picker
    }

    pub fn widget(&self) -> &gtk::ColorDialogButton {
        &self.0
    }

    pub fn set_value(&self, value: &str) {
        if let Ok(color) = gtk::gdk::RGBA::parse(value) {
            self.0.set_rgba(&color);
        }
    }

    pub fn value(&self) -> String {
        let color = self.0.rgba();
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
        self.0
            .set_font_desc(&gtk::pango::FontDescription::from_string(family));
    }

    pub fn value(&self) -> String {
        self.0
            .font_desc()
            .and_then(|description| description.family().map(|family| family.to_string()))
            .unwrap_or_default()
    }
}
