use super::*;
use crate::components::layout::Panel;

impl Overview<'_> {
    /// Editable workspace settings. Identity remains fixed after workspace creation.
    pub(super) fn settings(&self) -> gtk::ScrolledWindow {
        let workspace = self.workspace;
        let form = Rc::new(Form::new());
        let terminal = workspace.terminal_config();
        form.font.set_value(&terminal.font_family);
        form.font_size.set_value(terminal.font_size);
        form.foreground.set_value(&terminal.foreground);
        form.background.set_value(&terminal.background);
        form.cursor.set(terminal.cursor_shape);
        form.cursor_blink.set_active(terminal.cursor_blink);

        for (key, value) in &workspace.env {
            form.add_environment();
            if let Some((key_input, value_input)) = form.env_rows.borrow().last() {
                key_input.set_text(key);
                value_input.set_text(value);
            }
        }
        for mount in &workspace.mounts {
            form.add_mount();
            if let Some((host, container, read_only)) = form.mount_rows.borrow().last() {
                host.set_text(&mount.host);
                container.set_text(&mount.container);
                read_only.set_active(mount.ro);
            }
        }

        let main = gtk::Box::new(gtk::Orientation::Vertical, 18);
        main.add_css_class("dmain");
        main.add_css_class("workspace-settings");
        main.append(&self.header());

        main.append(&self.identity());
        main.append(&Self::apply_note());

        let shell = Self::card("Shell", Some("Choose what starts when you open a new terminal tab."));
        shell.append(&Field::text(
            "DEFAULT SHELL",
            &form.shell,
            Some("Blank = auto (bash -il, else sh -i)."),
        ));

        let sections = gtk::FlowBox::new();
        sections.add_css_class("settings-grid");
        sections.set_selection_mode(gtk::SelectionMode::None);
        sections.set_min_children_per_line(1);
        sections.set_max_children_per_line(2);
        sections.set_column_spacing(14);
        sections.set_row_spacing(14);
        sections.set_homogeneous(false);
        for card in [
            shell,
            Self::decorate(form.terminal(), "Terminal appearance and history for each new tab."),
            Self::decorate(form.resources(), "Optional limits for workloads in this workspace."),
            Self::decorate(form.environment(), "Variables inherited by every new shell."),
            Self::decorate(form.mounts(), "Host folders made available inside the workspace."),
            Self::decorate(form.docker(), "Control access to the host-compatible Docker API."),
            Self::decorate(form.network(), "Configure how this workspace reaches private networks."),
        ] {
            sections.insert(&card, -1);
        }
        main.append(&sections);

        Self::populate(&form, workspace);
        main.append(&Self::save_row(form));

        gtk::ScrolledWindow::builder()
            .child(&main)
            .hexpand(true)
            .vexpand(true)
            .build()
    }

    fn header(&self) -> gtk::Box {
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let title = gtk::Label::new(Some("Settings"));
        title.add_css_class("dashtitle");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        header.append(&title);
        header
    }

    /// The workspace coordinates that cannot be changed after creation.
    fn identity(&self) -> gtk::Box {
        let card = Self::card(
            "Workspace identity",
            Some("Name, image, and architecture are fixed after creation."),
        );
        card.add_css_class("settings-identity");

        let values = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        values.add_css_class("settings-identity-values");
        let name = gtk::Label::new(Some(&self.workspace.name));
        name.add_css_class("settings-workspace-name");
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let image = gtk::Label::new(Some(&self.workspace.image));
        image.add_css_class("settings-image");
        image.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        image.set_tooltip_text(Some(&self.workspace.image));
        values.append(&name);
        values.append(&image);
        values.append(&ArchitectureView(self.workspace.arch).chip());
        card.append(&values);
        card
    }

    /// Sets expectations before a person edits a field, rather than only after saving.
    fn apply_note() -> gtk::Box {
        let note = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        note.add_css_class("settings-apply-note");
        let icon = gtk::Image::from_icon_name("dialog-information-symbolic");
        icon.set_valign(gtk::Align::Start);
        let copy = gtk::Label::new(Some(
            "Saved changes apply to newly opened tabs and future launches. Running tabs keep their current settings.",
        ));
        copy.set_xalign(0.0);
        copy.set_wrap(true);
        copy.set_hexpand(true);
        note.append(&icon);
        note.append(&copy);
        note
    }

    /// A settings card with an optional plain-language description.
    fn card(title: &str, description: Option<&str>) -> gtk::Box {
        let card = Panel::new(title).into_widget();
        card.add_css_class("settings-card");
        card.set_hexpand(true);
        card.set_valign(gtk::Align::Start);
        if let Some(description) = description {
            let copy = gtk::Label::new(Some(description));
            copy.add_css_class("settings-card-description");
            copy.set_xalign(0.0);
            copy.set_wrap(true);
            card.insert_child_after(&copy, card.first_child().as_ref());
        }
        card
    }

    /// Applies settings-page card treatment to a shared workspace form panel.
    fn decorate(card: gtk::Box, description: &str) -> gtk::Box {
        card.add_css_class("settings-card");
        card.set_hexpand(true);
        card.set_valign(gtk::Align::Start);
        let copy = gtk::Label::new(Some(description));
        copy.add_css_class("settings-card-description");
        copy.set_xalign(0.0);
        copy.set_wrap(true);
        card.insert_child_after(&copy, card.first_child().as_ref());
        card
    }

    fn populate(form: &Form, workspace: &WorkspaceConfig) {
        form.name.set_text(&workspace.name);
        form.image.set_text(&workspace.image);
        form.cpu_amd.set(workspace.arch == Arch::Amd64);
        form.shell.set_text(workspace.shell.as_deref().unwrap_or_default());
        if let Some(storage) = workspace.storage.as_deref() {
            form.storage.set_text(&storage.to_string_lossy());
        } else {
            form.storage.set_text("");
        }
        form.cpus.set_value(workspace.cpus.unwrap_or(0) as f64);
        form.mem.set_value(workspace.memory_mb.unwrap_or(0) as f64);
        form.scrollback.set_text(&Self::scrollback_text(workspace.scrollback));
        form.features.docker.set_active(workspace.docker_sock);
        if let Some(vpn) = &workspace.vpn {
            form.features.vpn.set_text(&vpn.to_spec());
        }
    }

    fn scrollback_text(scrollback: Option<u64>) -> String {
        scrollback.map_or_else(|| "unlimited".to_owned(), |lines| lines.to_string())
    }

    fn save_row(form: Rc<Form>) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("settings-save-row");
        let status = gtk::Label::new(None);
        status.add_css_class("fhint");
        status.set_xalign(0.0);
        status.set_hexpand(true);

        let save = gtk::Button::with_label("Save changes");
        save.add_css_class("btn");
        save.add_css_class("primary");
        save.set_halign(gtk::Align::End);
        {
            let status = status.clone();
            save.connect_clicked(move |_| {
                let result = form
                    .configuration()
                    .and_then(|workspace| WorkspaceStore::load(Home::current().workspaces_config())?.upsert(workspace));
                match result {
                    Ok(()) => {
                        status.remove_css_class("err");
                        status.set_text("Saved — applies to newly-opened tabs (⌘T) and future launches.");
                    }
                    Err(error) => {
                        status.add_css_class("err");
                        status.set_text(&error.to_string());
                    }
                }
            });
        }

        row.append(&status);
        row.append(&save);
        row
    }
}

#[cfg(test)]
mod tests {
    use super::Overview;
    use crate::{Arch, WorkspaceConfig};
    use gtk::prelude::*;

    #[test]
    fn unlimited_scrollback_is_populated_explicitly() {
        assert_eq!(Overview::scrollback_text(None), "unlimited");
        assert_eq!(Overview::scrollback_text(Some(100_000)), "100000");
    }

    #[test]
    fn settings_present_identity_apply_semantics_and_responsive_cards() {
        if !crate::test_support::on_the_toolkit_thread(|| {
            let workspace = WorkspaceConfig::new("design system", "ghcr.io/acme/dev:2026.09", Arch::Amd64);
            let page = Overview::new(&workspace, None).settings();
            let widgets = descendants(page.upcast_ref());

            let text: Vec<String> = widgets
                .iter()
                .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
                .map(|label| label.text().to_string())
                .collect();
            assert!(
                text.iter().any(|line| line == "design system"),
                "workspace name is visible"
            );
            assert!(
                text.iter().any(|line| line == "ghcr.io/acme/dev:2026.09"),
                "image identity is visible"
            );
            assert!(
                text.iter()
                    .any(|line| line.contains("Running tabs keep their current settings")),
                "apply timing is explained before saving"
            );

            let grid = widgets
                .iter()
                .find(|widget| widget.has_css_class("settings-grid"))
                .and_then(|widget| widget.downcast_ref::<gtk::FlowBox>())
                .expect("settings has a responsive card grid");
            assert_eq!(grid.min_children_per_line(), 1);
            assert_eq!(grid.max_children_per_line(), 2);
            assert_eq!(grid.observe_children().n_items(), 7);
            assert!(
                widgets.iter().any(|widget| widget.has_css_class("settings-save-row")),
                "save action is visually separated from editable cards"
            );
        }) {
            eprintln!("skipped: no display connection");
        }
    }

    fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut found = vec![root.clone()];
        let mut child = root.first_child();
        while let Some(widget) = child {
            found.extend(descendants(&widget));
            child = widget.next_sibling();
        }
        found
    }
}
