pub mod create;
pub mod extension;
pub mod extensions;
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
    Extensions,
    Settings,
}

impl Page {
    pub const ALL: [Self; 8] = [
        Self::Overview,
        Self::Containers,
        Self::Images,
        Self::Volumes,
        Self::Networks,
        Self::Processes,
        Self::Extensions,
        Self::Settings,
    ];

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Containers => "Containers",
            Self::Images => "Images",
            Self::Volumes => "Volumes",
            Self::Networks => "Networks",
            Self::Processes => "Processes",
            Self::Extensions => "Extensions",
            Self::Settings => "Settings",
        }
    }
}

/// Workspace screen shell. The application supplies live page content and owns side effects.
pub struct View {
    pub widget: gtk::Box,
    sidebar: gtk::Box,
    pages: gtk::Stack,
    items: Rc<RefCell<Vec<gtk::Button>>>,
}

impl View {
    #[must_use]
    /// Generic over the page count so adding a page is one entry in `ALL` and
    /// one at the call site, rather than an arity change every caller must
    /// absorb.
    pub fn new<const N: usize>(content: [(Page, gtk::Widget); N]) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 2);
        sidebar.add_css_class("dside");
        let pages = gtk::Stack::new();
        pages.set_hexpand(true);
        pages.set_vexpand(true);
        pages.set_transition_type(gtk::StackTransitionType::None);
        let items: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));

        for (index, (page, content)) in content.into_iter().enumerate() {
            let item = Self::entry(&pages, &items, page.title(), &content);
            if index == 0 {
                item.add_css_class("on");
            }
            sidebar.append(&item);
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

        Self {
            widget,
            sidebar,
            pages,
            items,
        }
    }

    /// Adds one page after the shell was built.
    ///
    /// The fixed pages are the product's own and are known at compile time; a
    /// workspace's extensions are neither, and one being installed must show up
    /// without the window being opened again.
    pub fn attach(&self, name: &str, content: &impl IsA<gtk::Widget>) {
        let item = Self::entry(&self.pages, &self.items, name, content.as_ref());
        self.sidebar.append(&item);
    }

    /// Removes a page added by [`View::attach`], with its sidebar entry.
    ///
    /// Removing a page that is not there does nothing, because the caller
    /// wanted it gone and it is.
    pub fn detach(&self, name: &str) {
        if let Some(content) = self.pages.child_by_name(name) {
            self.pages.remove(&content);
        }
        let Some(index) = self.items.borrow().iter().position(|item| Self::names(item, name)) else {
            return;
        };
        let item = self.items.borrow_mut().remove(index);
        self.sidebar.remove(&item);
    }

    /// Whether a page under this name is currently on the shell.
    #[must_use]
    pub fn holds(&self, name: &str) -> bool {
        self.pages.child_by_name(name).is_some()
    }

    /// The sidebar entries, in the order they are shown.
    #[must_use]
    pub fn entries(&self) -> Vec<String> {
        self.items
            .borrow()
            .iter()
            .filter_map(|item| item.label().map(|label| label.to_string()))
            .collect()
    }

    /// The page currently shown, by name.
    #[must_use]
    pub fn shown(&self) -> Option<String> {
        self.pages.visible_child_name().map(|name| name.to_string())
    }

    /// The widget shown under one name, for tests and diagnostics.
    #[must_use]
    pub fn page(&self, name: &str) -> Option<gtk::Widget> {
        self.pages.child_by_name(name)
    }

    /// One sidebar entry and the page it selects, which is the whole of what a
    /// page is on this shell.
    fn entry(
        pages: &gtk::Stack,
        items: &Rc<RefCell<Vec<gtk::Button>>>,
        name: &str,
        content: &gtk::Widget,
    ) -> gtk::Button {
        pages.add_named(content, Some(name));
        let item = gtk::Button::with_label(name);
        item.add_css_class("dsi");
        item.set_has_frame(false);
        item.set_hexpand(true);
        item.set_halign(gtk::Align::Fill);
        if let Some(label) = item.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        let stack = pages.clone();
        let event_items = items.clone();
        // An attached page is named at runtime, so the name is owned rather
        // than borrowed from the fixed page list.
        let selected = name.to_owned();
        item.connect_clicked(move |_| {
            stack.set_visible_child_name(&selected);
            Self::select_items(&event_items.borrow(), &selected);
        });
        items.borrow_mut().push(item.clone());
        item
    }

    pub fn select_name(&self, name: &str) {
        self.pages.set_visible_child_name(name);
        Self::select_items(&self.items.borrow(), name);
    }

    fn select_items(items: &[gtk::Button], selected: &str) {
        for item in items {
            if Self::names(item, selected) {
                item.add_css_class("on");
            } else {
                item.remove_css_class("on");
            }
        }
    }

    /// Whether one sidebar entry is the entry for this page.
    fn names(item: &gtk::Button, name: &str) -> bool {
        item.label().as_deref() == Some(name)
    }
}
