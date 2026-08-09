use super::*;

impl Overview<'_> {
    pub(super) fn overview(&self) -> gtk::ScrolledWindow {
        let workspace = self.workspace;
        let main = gtk::Box::new(gtk::Orientation::Vertical, 10);
        main.add_css_class("dmain");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let name = gtk::Label::new(Some(&workspace.name));
        name.add_css_class("dashtitle");
        name.set_xalign(0.0);
        header.append(&name);
        header.append(&ArchitectureView(workspace.arch).chip());
        main.append(&header);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(9);
        grid.set_column_spacing(18);
        let mut row = 0i32;
        let mut append = |key: &str, value: String| {
            let key = gtk::Label::new(Some(key));
            key.add_css_class("kvk");
            key.set_xalign(0.0);
            key.set_valign(gtk::Align::Start);
            let value = gtk::Label::new(Some(&value));
            value.add_css_class("kvv");
            value.set_xalign(0.0);
            value.set_wrap(true);
            value.set_selectable(true);
            grid.attach(&key, 0, row, 1, 1);
            grid.attach(&value, 1, row, 1, 1);
            row += 1;
        };

        append("Image", workspace.image.clone());
        append("Architecture", workspace.arch.as_str().to_string());
        let home = Home::current();
        append("Storage", home.display(&workspace.storage_dir(&home.root())));
        append(
            "Shell",
            workspace.shell.clone().unwrap_or_else(|| "auto (bash → sh)".into()),
        );
        append(
            "CPU cores",
            workspace
                .cpus
                .map_or_else(|| "unlimited".into(), |cores| cores.to_string()),
        );
        append(
            "Memory",
            workspace
                .memory_mb
                .map_or_else(|| "unlimited".into(), |memory| format!("{memory} MB")),
        );
        append(
            "Docker socket",
            if workspace.docker_sock {
                "mounted (DOCKER_HOST set)".into()
            } else {
                "off".into()
            },
        );
        append(
            "VPN egress",
            workspace
                .vpn
                .as_ref()
                .map_or_else(|| "direct".into(), hl::config::VpnConfig::to_spec),
        );
        append(
            "CUDA device",
            workspace.cuda.as_ref().map_or_else(
                || "none".into(),
                |cuda| {
                    format!(
                        "{} (cc {}, {} MB) → host Metal",
                        cuda.name, cuda.compute_capability, cuda.vram_mb
                    )
                },
            ),
        );
        if !workspace.env.is_empty() {
            append(
                "Environment",
                workspace
                    .env
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        if !workspace.mounts.is_empty() {
            append(
                "Mounts",
                workspace
                    .mounts
                    .iter()
                    .map(|mount| {
                        format!(
                            "{} → {} ({})",
                            mount.host,
                            mount.container,
                            if mount.ro { "ro" } else { "rw" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        main.append(&grid);

        let tip = gtk::Label::new(Some("⌘T opens a shell in this workspace. ⌘D splits."));
        tip.add_css_class("dhint");
        tip.set_xalign(0.0);
        tip.set_margin_top(6);
        main.append(&tip);

        gtk::ScrolledWindow::builder()
            .child(&main)
            .hexpand(true)
            .vexpand(true)
            .build()
    }
}
