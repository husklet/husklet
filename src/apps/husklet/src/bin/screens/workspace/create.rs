#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Basics,
    Terminal,
    Integrations,
    Advanced,
}

impl Page {
    pub const ALL: [Self; 4] = [Self::Basics, Self::Terminal, Self::Integrations, Self::Advanced];

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Basics => "Basics",
            Self::Terminal => "Terminal",
            Self::Integrations => "Integrations",
            Self::Advanced => "Advanced",
        }
    }

    #[must_use]
    fn group_name(name: &str) -> &str {
        match name {
            "General" | "Resources" => "Basics",
            "Environment" | "Mounts" => "Integrations",
            "Docker" | "Network" => "Advanced",
            name => name,
        }
    }
}

/// Workspace creation shell. Form values and persistence remain application-owned.
pub struct View {
    pub widget: gtk::Box,
    pub pages: gtk::Stack,
    pub cancel: gtk::Button,
    pub create: gtk::Button,
    pub status: gtk::Label,
    navigation: Rc<RefCell<Vec<gtk::Button>>>,
}

impl View {
    #[must_use]
    pub fn new(content: [(Page, gtk::Box); 4]) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let split = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        split.set_vexpand(true);
        let nav = gtk::Box::new(gtk::Orientation::Vertical, 2);
        nav.add_css_class("nav");
        let pages = gtk::Stack::new();
        pages.set_hexpand(true);
        pages.set_vexpand(true);
        pages.set_transition_type(gtk::StackTransitionType::None);
        let navigation: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));

        for (index, (page, content)) in content.into_iter().enumerate() {
            let name = page.title();
            let scroller = gtk::ScrolledWindow::builder()
                .hexpand(true)
                .vexpand(true)
                .child(&content)
                .build();
            scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            pages.add_named(&scroller, Some(name));
            let button = gtk::Button::with_label(name);
            button.add_css_class("navi");
            button.set_has_frame(false);
            button.set_hexpand(true);
            button.set_halign(gtk::Align::Fill);
            if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.0);
            }
            if index == 0 {
                button.add_css_class("on");
            }
            let stack = pages.clone();
            let page_focus = scroller.clone();
            let event_navigation = navigation.clone();
            button.connect_clicked(move |_| {
                stack.set_visible_child_name(name);
                Self::select_navigation(&event_navigation.borrow(), name);
                page_focus.child_focus(gtk::DirectionType::TabForward);
            });
            nav.append(&button);
            navigation.borrow_mut().push(button);
        }

        split.append(&nav);
        split.append(&pages);
        widget.append(&split);

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        footer.add_css_class("footer");
        let status = gtk::Label::new(None);
        status.add_css_class("fhint");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        footer.append(&status);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("btn");
        let create = gtk::Button::with_label("Create workspace");
        create.add_css_class("btn");
        create.add_css_class("primary");
        footer.append(&cancel);
        footer.append(&create);
        widget.append(&footer);

        Self {
            widget,
            pages,
            cancel,
            create,
            status,
            navigation,
        }
    }

    pub fn select(&self, page: Page) {
        let name = page.title();
        self.pages.set_visible_child_name(name);
        Self::select_navigation(&self.navigation.borrow(), name);
    }

    pub fn select_name(&self, name: &str) {
        let grouped = Page::group_name(name);
        self.pages.set_visible_child_name(grouped);
        Self::select_navigation(&self.navigation.borrow(), grouped);
    }

    fn select_navigation(navigation: &[gtk::Button], selected: &str) {
        for button in navigation {
            if button.label().as_deref() == Some(selected) {
                button.add_css_class("on");
            } else {
                button.remove_css_class("on");
            }
        }
    }
}
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[cfg(test)]
mod tests {
    use super::Page;

    #[test]
    fn navigation_contains_only_supported_workspace_settings() {
        assert_eq!(
            Page::ALL.map(Page::title),
            ["Basics", "Terminal", "Integrations", "Advanced"]
        );
    }

    #[test]
    fn old_debug_page_names_select_their_task_group() {
        for (name, group) in [
            ("General", "Basics"),
            ("Resources", "Basics"),
            ("Environment", "Integrations"),
            ("Mounts", "Integrations"),
            ("Docker", "Advanced"),
            ("Network", "Advanced"),
            ("Terminal", "Terminal"),
        ] {
            assert_eq!(Page::group_name(name), group);
        }
    }
}
