use super::*;

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

        let main = gtk::Box::new(gtk::Orientation::Vertical, 14);
        main.add_css_class("dmain");
        main.append(&self.header());

        let identity = gtk::Label::new(Some(&format!(
            "{}  ·  image + architecture are fixed at creation",
            workspace.image
        )));
        identity.add_css_class("fhint");
        identity.set_xalign(0.0);
        main.append(&identity);

        main.append(&Field::text(
            "DEFAULT SHELL",
            &form.shell,
            Some("Blank = auto (bash -il, else sh -i)."),
        ));
        main.append(&form.terminal());
        main.append(&form.resources());
        main.append(&form.environment());
        main.append(&form.mounts());
        main.append(&form.docker());
        main.append(&form.network());

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
        header.append(&ArchitectureView(self.workspace.arch).chip());
        header
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

    #[test]
    fn unlimited_scrollback_is_populated_explicitly() {
        assert_eq!(Overview::scrollback_text(None), "unlimited");
        assert_eq!(Overview::scrollback_text(Some(100_000)), "100000");
    }
}
