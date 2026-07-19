use crate::*;

mod process;
mod resources;

pub(crate) use process::*;
pub(crate) use resources::*;

pub(crate) struct Overview<'a> {
    workspace: &'a WorkspaceConfig,
    page: Option<screens::workspace::Page>,
}

impl<'a> Overview<'a> {
    pub(crate) fn new(
        workspace: &'a WorkspaceConfig,
        page: Option<screens::workspace::Page>,
    ) -> Self {
        Self { workspace, page }
    }

    pub(crate) fn view(&self) -> gtk::Box {
        let ws = self.workspace;
        use screens::workspace::Page as WorkspacePage;

        // Live panes fed by a background poller over the workspace daemon's Unix socket.
        let data = std::sync::Arc::new(std::sync::Mutex::new(OverviewData::default()));
        spawn_overview_poller(ws.name.clone(), self.shell_label(), data.clone());
        let containers = Table::new(&["NAME", "IMAGE", "STATUS"]);
        let images = Table::new(&["REPOSITORY", "IMAGE ID", "SIZE"]);
        let volumes = Table::new(&["NAME", "DRIVER"]);
        let networks = Table::new(&["NAME", "DRIVER", "SCOPE"]);
        let (ppane, pbody) = live_proc_pane();
        let view = screens::workspace::View::new([
            (WorkspacePage::Overview, self.overview().upcast()),
            (
                WorkspacePage::Containers,
                containers.widget.clone().upcast(),
            ),
            (WorkspacePage::Images, images.widget.clone().upcast()),
            (WorkspacePage::Volumes, volumes.widget.clone().upcast()),
            (WorkspacePage::Networks, networks.widget.clone().upcast()),
            (WorkspacePage::Processes, ppane.upcast()),
            (WorkspacePage::Settings, self.settings().upcast()),
        ]);
        glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
            let d = data.lock().unwrap().clone();
            containers.fill(&d.containers, d.error.as_deref());
            images.fill(&d.images, d.error.as_deref());
            volumes.fill(&d.volumes, d.error.as_deref());
            networks.fill(&d.networks, d.error.as_deref());
            fill_proc_table(&pbody, &d.processes, d.error.as_deref());
            glib::ControlFlow::Continue
        });

        // Debug: HL_TERM_OVERVIEW_PAGE selects a overview pane for screenshotting.
        if let Some(p) = AppConfig::get().overview_pane.as_deref() {
            view.select_name(p);
        } else if let Some(page) = self.page {
            view.select_name(page.title());
        }
        view.widget
    }

    fn shell_label(&self) -> String {
        self.workspace
            .shell
            .as_deref()
            .map(str::trim)
            .filter(|shell| !shell.is_empty())
            .and_then(|shell| shell.split_whitespace().next())
            .map(|shell| shell.rsplit('/').next().unwrap_or(shell).to_string())
            .unwrap_or_else(|| "bash".to_string())
    }

    /// Editable workspace settings, reachable from the sidebar. Everything a workspace defines EXCEPT its
    /// identity (name / image / arch — set once at creation) can be changed here: default shell, resource
    /// caps, environment variables, bind mounts, and the docker socket. Saving rewrites `workspaces.conf`;
    /// changes apply to newly-launched tabs (a running container can't be reconfigured live).
    fn settings(&self) -> gtk::ScrolledWindow {
        let ws = self.workspace;
        let form = Rc::new(build_form());
        let terminal = ws.terminal_config();
        form.font.set_value(&terminal.font_family);
        form.font_size.set_value(terminal.font_size);
        form.foreground.set_value(&terminal.foreground);
        form.background.set_value(&terminal.background);
        form.cursor.set(terminal.cursor_shape);
        form.cursor_blink.set_active(terminal.cursor_blink);

        // Pre-populate env + mount rows BEFORE their panes wrap the boxes, so existing entries show first.
        for (k, v) in &ws.env {
            form.add_environment();
            if let Some((ke, ve)) = form.env_rows.borrow().last() {
                ke.set_text(k);
                ve.set_text(v);
            }
        }
        for m in &ws.mounts {
            form.add_mount();
            if let Some((h, c, ro)) = form.mount_rows.borrow().last() {
                h.set_text(&m.host);
                c.set_text(&m.container);
                ro.set_active(m.ro);
            }
        }

        let main = gtk::Box::new(gtk::Orientation::Vertical, 14);
        main.add_css_class("dmain");

        // Identity header — read-only (image/arch are creation-only).
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let nm = gtk::Label::new(Some("Settings"));
        nm.add_css_class("dashtitle");
        nm.set_xalign(0.0);
        nm.set_hexpand(true);
        head.append(&nm);
        head.append(&ArchitectureView(ws.arch).chip());
        main.append(&head);
        let idl = gtk::Label::new(Some(&format!(
            "{}  ·  image + architecture are fixed at creation",
            ws.image
        )));
        idl.add_css_class("fhint");
        idl.set_xalign(0.0);
        main.append(&idl);

        // Editable sections (reuse the new-workspace panes).
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
        main.append(&form.applications());
        main.append(&form.network());
        main.append(&form.compute());

        // Apply the workspace's values AFTER the pane builders (which set their own defaults).
        form.name.set_text(&ws.name);
        form.image.set_text(&ws.image);
        form.cpu_amd.set(ws.arch == Arch::Amd64);
        if let Some(s) = &ws.shell {
            form.shell.set_text(s);
        }
        if let Some(st) = &ws.storage {
            form.storage.set_text(&st.to_string_lossy());
        }
        form.cpus.set_value(ws.cpus.unwrap_or(0) as f64);
        form.mem.set_value(ws.memory_mb.unwrap_or(0) as f64);
        if let Some(sb) = ws.scrollback {
            form.scrollback.set_text(&sb.to_string());
        }
        form.features.docker.set_active(ws.docker_sock);
        form.features.graphical.set_active(ws.gui);
        if let Some(vpn) = &ws.vpn {
            form.features.vpn.set_text(&vpn.to_spec());
        }
        if let Some(cuda) = &ws.cuda {
            form.features.cuda.set_active(true);
            form.features.cuda_name.set_text(&cuda.name);
            form.features
                .cuda_capability
                .set_text(&cuda.compute_capability);
            form.features
                .cuda_memory
                .set_text(&cuda.vram_mb.to_string());
        }

        // Save row.
        let saverow = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let status = gtk::Label::new(None);
        status.add_css_class("fhint");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        let save = gtk::Button::with_label("Save changes");
        save.add_css_class("btn");
        save.add_css_class("primary");
        save.set_halign(gtk::Align::End);
        {
            let form = form.clone();
            let status = status.clone();
            save.connect_clicked(move |_| {
                if form.save() {
                    status.remove_css_class("err");
                    status
                        .set_text("Saved — applies to newly-opened tabs (⌘T) and future launches.");
                } else {
                    status.add_css_class("err");
                    status.set_text("Could not save — check the fields.");
                }
            });
        }
        saverow.append(&status);
        saverow.append(&save);
        main.append(&saverow);

        gtk::ScrolledWindow::builder()
            .child(&main)
            .hexpand(true)
            .vexpand(true)
            .build()
    }

    fn overview(&self) -> gtk::ScrolledWindow {
        let ws = self.workspace;
        let main = gtk::Box::new(gtk::Orientation::Vertical, 10);
        main.add_css_class("dmain");

        let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let nm = gtk::Label::new(Some(&ws.name));
        nm.add_css_class("dashtitle");
        nm.set_xalign(0.0);
        head.append(&nm);
        head.append(&ArchitectureView(ws.arch).chip());
        main.append(&head);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(9);
        grid.set_column_spacing(18);
        let mut row = 0i32;
        let mut kv = |k: &str, v: String| {
            let kl = gtk::Label::new(Some(k));
            kl.add_css_class("kvk");
            kl.set_xalign(0.0);
            kl.set_valign(gtk::Align::Start);
            let vl = gtk::Label::new(Some(&v));
            vl.add_css_class("kvv");
            vl.set_xalign(0.0);
            vl.set_wrap(true);
            vl.set_selectable(true);
            grid.attach(&kl, 0, row, 1, 1);
            grid.attach(&vl, 1, row, 1, 1);
            row += 1;
        };
        kv("Image", ws.image.clone());
        kv("Architecture", ws.arch.as_str().to_string());
        let home = Home::current();
        kv("Storage", home.display(&ws.storage_dir(&home.root())));
        kv(
            "Shell",
            ws.shell
                .clone()
                .unwrap_or_else(|| "auto (bash \u{2192} sh)".into()),
        );
        kv(
            "CPU cores",
            ws.cpus
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unlimited".into()),
        );
        kv(
            "Memory",
            ws.memory_mb
                .map(|m| format!("{m} MB"))
                .unwrap_or_else(|| "unlimited".into()),
        );
        kv(
            "Docker socket",
            if ws.docker_sock {
                "mounted (DOCKER_HOST set)".into()
            } else {
                "off".into()
            },
        );
        kv(
            "VPN egress",
            ws.vpn
                .as_ref()
                .map(|v| v.to_spec())
                .unwrap_or_else(|| "direct".into()),
        );
        kv(
            "CUDA device",
            ws.cuda
                .as_ref()
                .map(|c| {
                    format!(
                        "{} (cc {}, {} MB) \u{2192} host Metal",
                        c.name, c.compute_capability, c.vram_mb
                    )
                })
                .unwrap_or_else(|| "none".into()),
        );
        if !ws.env.is_empty() {
            kv(
                "Environment",
                ws.env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        if !ws.mounts.is_empty() {
            kv(
                "Mounts",
                ws.mounts
                    .iter()
                    .map(|m| {
                        format!(
                            "{} \u{2192} {} ({})",
                            m.host,
                            m.container,
                            if m.ro { "ro" } else { "rw" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        main.append(&grid);

        let tip = gtk::Label::new(Some(
            "\u{2318}T opens a shell in this workspace. \u{2318}D splits.",
        ));
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

/// Latest snapshot of the workspace daemon's resources (rows are pre-formatted cell strings).
#[derive(Default, Clone)]
struct OverviewData {
    containers: Vec<Vec<String>>,
    images: Vec<Vec<String>>,
    volumes: Vec<Vec<String>>,
    networks: Vec<Vec<String>>,
    processes: Vec<Vec<String>>,
    error: Option<String>,
}

/// Background thread: ensure the workspace daemon, then poll it over its Unix socket every ~2s.
fn spawn_overview_poller(
    ws_name: String,
    shell: String,
    data: std::sync::Arc<std::sync::Mutex<OverviewData>>,
) {
    std::thread::spawn(move || {
        // A private worker starts the workspace-owned resource service and returns its socket.
        let sock = Hl::command(&["daemon", &ws_name, ""])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string())
            });
        loop {
            let mut d = OverviewData::default();
            match sock.as_ref().filter(|p| !p.as_os_str().is_empty()) {
                Some(s) => {
                    let daemon = WorkspaceResources::new(s);
                    d.containers = daemon.containers();
                    d.images = daemon.images();
                    d.volumes = daemon.volumes();
                    d.networks = daemon.networks();
                }
                None => d.error = Some("workspace daemon unavailable".into()),
            }
            // Workspace processes = the launched shells + their guest subprocesses, read from the host
            // process table (they run in-process via hl-jit, NOT through the daemon).
            d.processes = WorkspaceProcesses::new(&ws_name, &shell).read();
            if let Ok(mut g) = data.lock() {
                *g = d;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

struct Table {
    widget: gtk::ScrolledWindow,
    body: gtk::Box,
}

impl Table {
    pub(crate) fn new(headers: &[&str]) -> Self {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.add_css_class("dmain");
        outer.append(&Self::row(headers, "thead"));
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_hexpand(true);
        outer.append(&body);
        let widget = gtk::ScrolledWindow::builder()
            .child(&outer)
            .hexpand(true)
            .vexpand(true)
            .build();
        Self { widget, body }
    }

    fn row(cells: &[&str], css: &str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("trow");
        row.add_css_class(css);
        for (index, cell) in cells.iter().enumerate() {
            let label = gtk::Label::new(Some(cell));
            label.set_xalign(0.0);
            label.set_hexpand(index == 0);
            label.set_width_chars(if index == 0 { 24 } else { 16 });
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("tcell");
            row.append(&label);
        }
        row
    }

    fn fill(&self, rows: &[Vec<String>], error: Option<&str>) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        if let Some(error) = error {
            let label = gtk::Label::new(Some(error));
            label.add_css_class("dhint");
            label.set_margin_top(16);
            self.body.append(&label);
            return;
        }
        if rows.is_empty() {
            let label = gtk::Label::new(Some("— none —"));
            label.add_css_class("dhint");
            label.set_margin_top(16);
            label.set_halign(gtk::Align::Start);
            self.body.append(&label);
            return;
        }
        for row in rows {
            let cells: Vec<&str> = row.iter().map(String::as_str).collect();
            self.body.append(&Self::row(&cells, "tbody"));
        }
    }
}
