use super::*;
use crate::components::layout::Panel;

impl Overview<'_> {
    /// Editable workspace settings. Identity remains fixed after workspace creation.
    pub(super) fn settings(&self, semantics: &screens::workspace::semantic::Registry) -> gtk::ScrolledWindow {
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
        let save = Self::save_row(Rc::clone(&form), workspace.clone());
        main.append(&save.0);
        Self::register_semantics(semantics, &form, &save.1, workspace);

        gtk::ScrolledWindow::builder()
            .child(&main)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
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

    fn save_row(form: Rc<Form>, initial: WorkspaceConfig) -> (gtk::Box, gtk::Button) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("settings-save-row");
        let status = gtk::Label::new(Some("No unsaved changes."));
        status.add_css_class("fhint");
        status.set_xalign(0.0);
        status.set_hexpand(true);

        let save = gtk::Button::with_label("Save changes");
        save.add_css_class("btn");
        save.add_css_class("primary");
        save.set_halign(gtk::Align::End);
        save.set_sensitive(false);
        let saved = Rc::new(RefCell::new(initial));
        let dirty = Rc::new(Cell::new(false));
        {
            let status = status.clone();
            let save_button = save.clone();
            let form = Rc::clone(&form);
            let saved = Rc::clone(&saved);
            let dirty = Rc::clone(&dirty);
            save.connect_clicked(move |_| {
                let result = form.configuration().and_then(|workspace| {
                    WorkspaceStore::load(Home::current().workspaces_config())?.upsert(workspace.clone())?;
                    Ok(workspace)
                });
                match result {
                    Ok(workspace) => {
                        *saved.borrow_mut() = workspace;
                        dirty.set(false);
                        save_button.set_sensitive(false);
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
        {
            let save = save.downgrade();
            let status = status.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let Some(save) = save.upgrade() else {
                    return gtk::glib::ControlFlow::Break;
                };
                let changed = form
                    .configuration()
                    .map_or(true, |current| current != *saved.borrow());
                if changed != dirty.get() {
                    dirty.set(changed);
                    save.set_sensitive(changed);
                    status.remove_css_class("err");
                    status.set_text(if changed { "Unsaved changes." } else { "No unsaved changes." });
                }
                gtk::glib::ControlFlow::Continue
            });
        }

        row.append(&status);
        row.append(&save);
        (row, save)
    }

    fn register_semantics(
        semantics: &screens::workspace::semantic::Registry,
        form: &Rc<Form>,
        save: &gtk::Button,
        workspace: &WorkspaceConfig,
    ) {
        use screens::workspace::semantic::{ActionKind, Value};

        register_text(semantics, "settings/shell", "Default shell", &form.shell, false);
        register_text(semantics, "settings/scrollback", "Scrollback", &form.scrollback, false);
        register_text(semantics, "settings/vpn", "VPN or proxy", &form.features.vpn, true);
        register_spin(semantics, "settings/cpus", "CPU cores", &form.cpus);
        register_spin(semantics, "settings/memory", "Memory MB", &form.mem);
        register_spin(semantics, "settings/font-size", "Font size", &form.font_size);
        register_switch(semantics, "settings/cursor-blink", "Cursor blink", &form.cursor_blink);
        register_switch(semantics, "settings/docker", "Docker socket", &form.features.docker);

        semantics.register(
            "settings/workspace-name",
            "text",
            Some("Workspace name"),
            Some(Value::Public(&workspace.name)),
            &[],
            Rc::new(|_, _| {}),
        );
        semantics.register(
            "settings/image",
            "text",
            Some("Workspace image"),
            Some(Value::Public(&workspace.image)),
            &[],
            Rc::new(|_, _| {}),
        );
        for (index, (key, value)) in form.env_rows.borrow().iter().enumerate() {
            register_text(
                semantics,
                &format!("settings/environment/{index}/key"),
                "Environment key",
                key,
                false,
            );
            register_text(
                semantics,
                &format!("settings/environment/{index}/value"),
                "Environment value",
                value,
                true,
            );
        }
        for (index, (host, container, read_only)) in form.mount_rows.borrow().iter().enumerate() {
            register_text(
                semantics,
                &format!("settings/mount/{index}/host"),
                "Host path",
                host,
                true,
            );
            register_text(
                semantics,
                &format!("settings/mount/{index}/container"),
                "Container path",
                container,
                false,
            );
            register_toggle(
                semantics,
                &format!("settings/mount/{index}/read-only"),
                "Read only",
                read_only,
            );
        }
        let button = save.clone();
        semantics.register(
            "settings/save",
            "button",
            Some("Save changes"),
            None,
            &[ActionKind::Invoke],
            Rc::new(move |_, _| button.emit_clicked()),
        );
        semantics.set_disabled("settings/save", !save.is_sensitive());
        let registry = semantics.clone();
        save.connect_sensitive_notify(move |button| registry.set_disabled("settings/save", !button.is_sensitive()));
    }
}

fn register_text(
    semantics: &screens::workspace::semantic::Registry,
    path: &str,
    label: &str,
    input: &gtk::Entry,
    secret: bool,
) {
    use screens::workspace::semantic::{ActionKind, Value};
    let initial = input.text();
    let value = if secret {
        Value::Secret
    } else {
        Value::Public(initial.as_str())
    };
    let changed = input.clone();
    let focused = input.clone();
    semantics.register(
        path,
        "textbox",
        Some(label),
        Some(value),
        &[ActionKind::Change, ActionKind::Focus],
        Rc::new(move |action, value| match action {
            ActionKind::Change => changed.set_text(value.unwrap_or_default()),
            ActionKind::Focus => {
                focused.grab_focus();
            }
            _ => {}
        }),
    );
    let registry = semantics.clone();
    let path = path.to_owned();
    input.connect_changed(move |input| {
        let text = input.text();
        registry.update(
            &path,
            if secret {
                Value::Secret
            } else {
                Value::Public(text.as_str())
            },
            !input.is_sensitive(),
        );
    });
}

fn register_spin(semantics: &screens::workspace::semantic::Registry, path: &str, label: &str, input: &gtk::SpinButton) {
    use screens::workspace::semantic::{ActionKind, Value};
    let initial = input.value().to_string();
    let changed = input.clone();
    semantics.register(
        path,
        "spinbutton",
        Some(label),
        Some(Value::Public(&initial)),
        &[ActionKind::Change, ActionKind::Focus],
        Rc::new(move |action, value| match action {
            ActionKind::Change => {
                if let Some(value) = value.and_then(|value| value.parse().ok()) {
                    changed.set_value(value);
                }
            }
            ActionKind::Focus => {
                changed.grab_focus();
            }
            _ => {}
        }),
    );
    let registry = semantics.clone();
    let path = path.to_owned();
    input.connect_value_changed(move |input| {
        let value = input.value().to_string();
        registry.update(&path, Value::Public(&value), !input.is_sensitive());
    });
}

fn register_switch(semantics: &screens::workspace::semantic::Registry, path: &str, label: &str, input: &gtk::Switch) {
    use screens::workspace::semantic::{ActionKind, Value};
    let initial = input.is_active().to_string();
    let changed = input.clone();
    semantics.register(
        path,
        "switch",
        Some(label),
        Some(Value::Public(&initial)),
        &[ActionKind::Toggle],
        Rc::new(move |_, _| changed.set_active(!changed.is_active())),
    );
    let registry = semantics.clone();
    let path = path.to_owned();
    input.connect_active_notify(move |input| {
        let value = input.is_active().to_string();
        registry.update(&path, Value::Public(&value), !input.is_sensitive());
    });
}

fn register_toggle(
    semantics: &screens::workspace::semantic::Registry,
    path: &str,
    label: &str,
    input: &gtk::CheckButton,
) {
    use screens::workspace::semantic::{ActionKind, Value};
    let initial = input.is_active().to_string();
    let changed = input.clone();
    semantics.register(
        path,
        "checkbox",
        Some(label),
        Some(Value::Public(&initial)),
        &[ActionKind::Toggle],
        Rc::new(move |_, _| changed.set_active(!changed.is_active())),
    );
    let registry = semantics.clone();
    let path = path.to_owned();
    input.connect_toggled(move |input| {
        let value = input.is_active().to_string();
        registry.update(&path, Value::Public(&value), !input.is_sensitive());
    });
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
            let page = Overview::new(&workspace, None)
                .settings(&crate::screens::workspace::semantic::Registry::new("workspace"));
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

            let window = gtk::Window::builder()
                .default_width(300)
                .default_height(700)
                .child(&page)
                .build();
            window.present();
            while gtk::glib::MainContext::default().iteration(false) {}
            let horizontal = page.hadjustment();
            assert!(
                horizontal.upper() <= horizontal.page_size() + 1.0,
                "a narrow settings page must not require horizontal scrolling: upper={} page_size={}",
                horizontal.upper(),
                horizontal.page_size()
            );
            window.close();
            assert!(
                widgets.iter().any(|widget| widget.has_css_class("settings-save-row")),
                "save action is visually separated from editable cards"
            );
        }) {
            eprintln!("skipped: no display connection");
        }
    }

    #[test]
    fn settings_owner_registers_live_controls_and_redacts_sensitive_values() {
        if !crate::test_support::on_the_toolkit_thread(|| {
            use crate::screens::workspace::semantic::{Action, ActionKind, Registry};
            let workspace = WorkspaceConfig::new("semantic", "alpine:3.20", Arch::Amd64);
            let registry = Registry::new("workspace");
            let page = Overview::new(&workspace, None).settings(&registry);
            let snapshot = registry.snapshot();
            let labels: Vec<_> = snapshot
                .root
                .children
                .iter()
                .filter_map(|node| node.label.as_deref())
                .collect();
            assert!(labels.contains(&"Default shell"));
            assert!(labels.contains(&"CPU cores"));
            assert!(labels.contains(&"Docker socket"));
            assert!(labels.contains(&"Save changes"));
            let vpn = snapshot
                .root
                .children
                .iter()
                .find(|node| node.label.as_deref() == Some("VPN or proxy"))
                .unwrap();
            assert_eq!(vpn.value.as_deref(), Some("[redacted]"));
            let shell = snapshot
                .root
                .children
                .iter()
                .find(|node| node.label.as_deref() == Some("Default shell"))
                .unwrap();
            let save = descendants(page.upcast_ref())
                .into_iter()
                .find_map(|widget| {
                    widget
                        .downcast::<gtk::Button>()
                        .ok()
                        .filter(|button| button.label().as_deref() == Some("Save changes"))
                })
                .expect("settings has a save action");
            assert!(!save.is_sensitive(), "unchanged settings cannot be redundantly saved");
            registry
                .act(&Action {
                    revision: snapshot.revision,
                    node: shell.id,
                    action: ActionKind::Change,
                    value: Some("/bin/zsh -l".to_owned()),
                })
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while !save.is_sensitive() && std::time::Instant::now() < deadline {
                gtk::glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(save.is_sensitive(), "editing a setting exposes the pending save");
            let changed = registry.snapshot();
            assert!(changed.revision > snapshot.revision);
            assert_eq!(
                changed
                    .root
                    .children
                    .iter()
                    .find(|node| node.id == shell.id)
                    .and_then(|node| node.value.as_deref()),
                Some("/bin/zsh -l")
            );
            assert!(
                !changed
                    .root
                    .children
                    .iter()
                    .find(|node| node.label.as_deref() == Some("Save changes"))
                    .expect("save semantics remain live")
                    .disabled,
                "assistive actions see that saving is now available"
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
