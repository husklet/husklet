#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    General,
    Terminal,
    Resources,
    Environment,
    Mounts,
    Docker,
    Network,
}

impl Page {
    pub const ALL: [Self; 7] = [
        Self::General,
        Self::Terminal,
        Self::Resources,
        Self::Environment,
        Self::Mounts,
        Self::Docker,
        Self::Network,
    ];

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Terminal => "Terminal",
            Self::Resources => "Resources",
            Self::Environment => "Environment",
            Self::Mounts => "Mounts",
            Self::Docker => "Docker",
            Self::Network => "Network",
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
    labels: Rc<RefCell<Vec<gtk::Label>>>,
}

impl View {
    #[must_use]
    pub fn new(content: [(Page, gtk::Box); 7]) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let split = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        split.set_vexpand(true);
        let nav = gtk::Box::new(gtk::Orientation::Vertical, 2);
        nav.add_css_class("nav");
        let pages = gtk::Stack::new();
        pages.set_hexpand(true);
        pages.set_vexpand(true);
        pages.set_transition_type(gtk::StackTransitionType::None);
        let labels: Rc<RefCell<Vec<gtk::Label>>> = Rc::new(RefCell::new(Vec::new()));

        for (index, (page, content)) in content.into_iter().enumerate() {
            let name = page.title();
            let scroller = gtk::ScrolledWindow::builder()
                .hexpand(true)
                .vexpand(true)
                .child(&content)
                .build();
            scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            pages.add_named(&scroller, Some(name));
            let label = gtk::Label::new(Some(name));
            label.add_css_class("navi");
            label.set_xalign(0.0);
            if index == 0 {
                label.add_css_class("on");
            }
            let click = gtk::GestureClick::new();
            let stack = pages.clone();
            let page_focus = scroller.clone();
            let event_labels = labels.clone();
            click.connect_released(move |_, _, _, _| {
                stack.set_visible_child_name(name);
                Self::select_labels(&event_labels.borrow(), name);
                page_focus.child_focus(gtk::DirectionType::TabForward);
            });
            label.add_controller(click);
            nav.append(&label);
            labels.borrow_mut().push(label);
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
            labels,
        }
    }

    pub fn select(&self, page: Page) {
        let name = page.title();
        self.pages.set_visible_child_name(name);
        Self::select_labels(&self.labels.borrow(), name);
    }

    pub fn select_name(&self, name: &str) {
        self.pages.set_visible_child_name(name);
        Self::select_labels(&self.labels.borrow(), name);
    }

    fn select_labels(labels: &[gtk::Label], selected: &str) {
        for label in labels {
            if label.text() == selected {
                label.add_css_class("on");
            } else {
                label.remove_css_class("on");
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
            [
                "General",
                "Terminal",
                "Resources",
                "Environment",
                "Mounts",
                "Docker",
                "Network"
            ]
        );
    }
}
