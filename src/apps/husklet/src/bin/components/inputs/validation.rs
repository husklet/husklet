use gtk::prelude::*;

use crate::components::workspace::Form;

pub(crate) struct FormValidation;

impl FormValidation {
    pub(crate) fn mark_required(form: &Form, name_valid: bool, image_valid: bool) {
        if !name_valid {
            form.name.add_css_class("err");
        }
        if !image_valid {
            form.image.add_css_class("err");
        }
    }

    pub(crate) fn focus_missing(form: &Form, name_valid: bool) {
        if name_valid {
            form.image.grab_focus();
        } else {
            form.name.grab_focus();
        }
    }
}
