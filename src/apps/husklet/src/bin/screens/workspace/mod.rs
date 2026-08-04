pub mod create;
pub mod overview;
pub mod settings;
pub mod terminal;

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Overview,
    Containers,
    Images,
    Volumes,
    Networks,
    Processes,
    Settings,
}

impl Page {
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::Containers,
        Self::Images,
        Self::Volumes,
        Self::Networks,
        Self::Processes,
        Self::Settings,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Containers => "Containers",
            Self::Images => "Images",
            Self::Volumes => "Volumes",
            Self::Networks => "Networks",
            Self::Processes => "Processes",
            Self::Settings => "Settings",
        }
    }
}

/// Workspace screen shell. The application supplies live page content and owns side effects.
pub struct View {
    pub widget: gtk::Box,
    pages: gtk::Stack,
    items: Rc<RefCell<Vec<gtk::Box>>>,
}

impl View {
    pub fn new(content: [(Page, gtk::Widget); 7]) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 2);
        sidebar.add_css_class("dside");
        let pages = gtk::Stack::new();
        pages.set_hexpand(true);
        pages.set_vexpand(true);
        pages.set_transition_type(gtk::StackTransitionType::None);
        let items: Rc<RefCell<Vec<gtk::Box>>> = Rc::new(RefCell::new(Vec::new()));

        for (index, (page, content)) in content.into_iter().enumerate() {
            let name = page.title();
            pages.add_named(&content, Some(name));
            let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            item.add_css_class("dsi");
            if index == 0 {
                item.add_css_class("on");
            }
            let label = gtk::Label::new(Some(name));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            item.append(&label);
            let click = gtk::GestureClick::new();
            let stack = pages.clone();
            let event_items = items.clone();
            click.connect_released(move |_, _, _, _| {
                stack.set_visible_child_name(name);
                Self::select_items(&event_items.borrow(), name);
            });
            item.add_controller(click);
            sidebar.append(&item);
            items.borrow_mut().push(item);
        }

        let split = gtk::Paned::new(gtk::Orientation::Horizontal);
        split.set_hexpand(true);
        split.set_vexpand(true);
        split.set_wide_handle(false);
        split.set_start_child(Some(&sidebar));
        split.set_end_child(Some(&pages));
        split.set_position(190);
        split.set_resize_start_child(false);
        split.set_shrink_start_child(false);
        widget.append(&split);

        Self { widget, pages, items }
    }

    pub fn select_name(&self, name: &str) {
        self.pages.set_visible_child_name(name);
        Self::select_items(&self.items.borrow(), name);
    }

    fn select_items(items: &[gtk::Box], selected: &str) {
        for item in items {
            let matches = item
                .first_child()
                .and_downcast::<gtk::Label>()
                .is_some_and(|label| label.text() == selected);
            if matches {
                item.add_css_class("on");
            } else {
                item.remove_css_class("on");
            }
        }
    }
}
