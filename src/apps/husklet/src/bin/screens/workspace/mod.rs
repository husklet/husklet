pub mod create;
pub mod extension;
pub mod extensions;
pub mod overview;
pub mod semantic;
pub mod terminal;

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Workspace,
    Extensions,
}

impl Page {
    pub const ALL: [Self; 2] = [Self::Workspace, Self::Extensions];

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Extensions => "Extensions",
        }
    }

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Extensions => "extensions",
        }
    }
}

/// Workspace screen shell. The application supplies live page content and owns side effects.
pub struct View {
    pub widget: gtk::Box,
    sidebar: gtk::Box,
    pages: gtk::Stack,
    items: Rc<RefCell<Vec<gtk::Button>>>,
    semantics: semantic::Registry,
}

impl View {
    #[must_use]
    /// Generic over the page count so adding a page is one entry in `ALL` and
    /// one at the call site, rather than an arity change every caller must
    /// absorb.
    pub fn new<const N: usize>(content: [(Page, gtk::Widget); N]) -> Self {
        Self::with_semantics(content, semantic::Registry::new("workspace"))
    }

    #[must_use]
    pub fn with_semantics<const N: usize>(content: [(Page, gtk::Widget); N], semantics: semantic::Registry) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 2);
        sidebar.add_css_class("dside");
        let pages = gtk::Stack::new();
        pages.set_hexpand(true);
        pages.set_vexpand(true);
        pages.set_transition_type(gtk::StackTransitionType::None);
        let items: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));
        for (index, (page, content)) in content.into_iter().enumerate() {
            let item = Self::entry(&pages, &items, &semantics, page.title(), page.title(), &content);
            if index == 0 {
                item.add_css_class("on");
            }
            sidebar.append(&item);
        }
        if let Some(first) = items.borrow().first().and_then(gtk::Button::label) {
            semantics.select(&Self::semantic_path(&first));
        }

        let split = gtk::Paned::new(gtk::Orientation::Horizontal);
        split.set_hexpand(true);
        split.set_vexpand(true);
        split.set_wide_handle(false);
        split.set_start_child(Some(&sidebar));
        split.set_end_child(Some(&pages));
        split.set_position(110);
        split.set_resize_start_child(false);
        // The fixed navigation contains only two short destinations. Keep it
        // compact so settings fields remain usable in a narrow workspace.
        split.set_shrink_start_child(true);
        widget.append(&split);

        Self {
            widget,
            sidebar,
            pages,
            items,
            semantics,
        }
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

    /// Adds or replaces one extension-owned overview page.
    pub fn attach(&self, name: &str, title: &str, content: &gtk::Widget) {
        self.detach(name);
        let item = Self::entry(&self.pages, &self.items, &self.semantics, name, title, content);
        self.sidebar.append(&item);
        {
            let mut items = self.items.borrow_mut();
            items.sort_by(|left, right| {
                navigation_order(&left.widget_name()).cmp(&navigation_order(&right.widget_name()))
            });
            let mut previous: Option<gtk::Button> = None;
            for button in items.iter() {
                self.sidebar.reorder_child_after(button, previous.as_ref());
                previous = Some(button.clone());
            }
        }
        if self.items.borrow().len() == 1 {
            item.add_css_class("on");
            self.pages.set_visible_child_name(name);
            self.semantics.select(&Self::semantic_path(name));
        }
    }

    /// Removes one extension-owned overview page and its navigation authority.
    pub fn detach(&self, name: &str) {
        if let Some(page) = self.pages.child_by_name(name) {
            self.pages.remove(&page);
        }
        let mut items = self.items.borrow_mut();
        if let Some(index) = items.iter().position(|item| Self::names(item, name)) {
            self.sidebar.remove(&items[index]);
            items.remove(index);
        }
        self.semantics.remove(&Self::semantic_path(name));
        if self.pages.visible_child().is_none() {
            if let Some(first) = items.first() {
                let id = first.widget_name();
                self.pages.set_visible_child_name(&id);
                first.add_css_class("on");
                self.semantics.select(&Self::semantic_path(&id));
            }
        }
    }

    /// One sidebar entry and the page it selects, which is the whole of what a
    /// page is on this shell.
    fn entry(
        pages: &gtk::Stack,
        items: &Rc<RefCell<Vec<gtk::Button>>>,
        semantics: &semantic::Registry,
        name: &str,
        title: &str,
        content: &gtk::Widget,
    ) -> gtk::Button {
        pages.add_named(content, Some(name));
        let item = gtk::Button::with_label(title);
        item.set_widget_name(name);
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
        let clicked_registry = semantics.clone();
        item.connect_clicked(move |_| {
            stack.set_visible_child_name(&selected);
            Self::select_items(&event_items.borrow(), &selected);
            clicked_registry.select(&Self::semantic_path(&selected));
        });
        let stack = pages.clone();
        let semantic_items = items.clone();
        let selected = name.to_owned();
        let registry = semantics.clone();
        let path = Self::semantic_path(name);
        semantics.register(
            &path,
            "tab",
            Some(title),
            Some(semantic::Value::Public("false")),
            &[semantic::ActionKind::Invoke, semantic::ActionKind::Focus],
            Rc::new(move |_, _| {
                stack.set_visible_child_name(&selected);
                Self::select_items(&semantic_items.borrow(), &selected);
                registry.select(&Self::semantic_path(&selected));
            }),
        );
        items.borrow_mut().push(item.clone());
        item
    }

    pub fn select_name(&self, name: &str) {
        self.pages.set_visible_child_name(name);
        Self::select_items(&self.items.borrow(), name);
        self.semantics.select(&Self::semantic_path(name));
    }

    /// Snapshot of product-owned workspace navigation, without GTK scraping.
    #[must_use]
    pub fn semantic_snapshot(&self) -> semantic::Snapshot {
        self.semantics.snapshot()
    }

    /// Applies a revision-checked semantic action to native workspace UI.
    pub fn semantic_action(&self, action: &semantic::Action) -> Result<(), semantic::Refusal> {
        self.semantics.act(action)
    }

    pub(crate) fn semantic_registry(&self) -> semantic::Registry {
        self.semantics.clone()
    }

    fn semantic_path(name: &str) -> String {
        format!("workspace/page/{name}")
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
        item.widget_name().as_str() == name
    }
}

#[cfg(test)]
mod semantic_tests {
    use super::*;

    #[test]
    fn native_pages_are_revisioned_and_actionable_without_widget_discovery() {
        if !crate::test_support::on_the_toolkit_thread(|| {
            let view = View::new([
                (Page::Extensions, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
                (Page::Workspace, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
            ]);
            let first = view.semantic_snapshot();
            assert_eq!(first.root.role, "navigation");
            assert_eq!(first.root.children.len(), 2);
            let extensions = first
                .root
                .children
                .iter()
                .find(|node| node.label.as_deref() == Some("Extensions"))
                .expect("extensions is registered by its owner");
            assert_eq!(extensions.value.as_deref(), Some("true"));

            let settings = first
                .root
                .children
                .iter()
                .find(|node| node.label.as_deref() == Some("Workspace"))
                .expect("settings is registered by its owner");
            view.semantic_action(&semantic::Action {
                revision: first.revision,
                node: settings.id,
                action: semantic::ActionKind::Invoke,
                value: None,
            })
            .unwrap();
            assert_eq!(view.shown().as_deref(), Some("Workspace"));
            let selected = view.semantic_snapshot();
            assert!(selected.revision > first.revision);
            assert_eq!(
                selected
                    .root
                    .children
                    .iter()
                    .find(|node| node.label.as_deref() == Some("Workspace"))
                    .and_then(|node| node.value.as_deref()),
                Some("true")
            );

            assert_eq!(view.entries(), ["Extensions", "Workspace"]);
        }) {
            eprintln!("skipped: no display connection");
        }
    }

    #[test]
    fn extension_pages_attach_replace_and_detach_by_stable_identity() {
        if !crate::test_support::on_the_toolkit_thread(|| {
            let view = View::with_semantics([], semantic::Registry::new("workspace"));
            let workspace = gtk::Label::new(Some("workspace one"));
            let extensions = gtk::Label::new(Some("extensions"));
            view.attach("workspace", "Workspace", workspace.upcast_ref());
            view.attach("extensions", "Extensions", extensions.upcast_ref());
            assert_eq!(view.entries(), ["Workspace", "Extensions"]);
            assert_eq!(view.shown().as_deref(), Some("workspace"));

            let replacement = gtk::Label::new(Some("workspace two"));
            view.attach("workspace", "Workspace", replacement.upcast_ref());
            assert_eq!(view.entries(), ["Workspace", "Extensions"]);
            assert_eq!(view.page("workspace"), Some(replacement.upcast()));

            view.detach("extensions");
            assert_eq!(view.entries(), ["Workspace"]);
            assert!(view.page("extensions").is_none());
        }) {
            eprintln!("skipped: no display connection");
        }
    }
}

fn navigation_order(name: &str) -> (u8, &str) {
    let rank = match name {
        "workspace" => 0,
        "extensions" => 1,
        _ => 2,
    };
    (rank, name)
}
