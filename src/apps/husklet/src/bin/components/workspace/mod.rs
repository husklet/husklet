use super::inputs::{FormValidation, ImagePicker, WorkspaceFeatureFields};
use super::layout::{Field, Panel};
use crate::*;

/// Controls shared by workspace creation and an existing workspace's settings tab.
/// Husklet owns their behavior; the toolkit widgets only collect and report values.
pub(crate) struct Form {
    pub(crate) name: gtk::Entry,
    pub(crate) image: gtk::Entry,
    pub(crate) shell: gtk::Entry,
    pub(crate) storage: gtk::Entry,
    pub(crate) cpu_amd: Rc<Cell<bool>>, // true = x86-64, false = arm64

    pub(crate) cpus: gtk::SpinButton,
    pub(crate) mem: gtk::SpinButton,
    pub(crate) scrollback: gtk::Entry,
    pub(crate) font: FontPicker,
    pub(crate) font_size: gtk::SpinButton,
    pub(crate) foreground: ColorPicker,
    pub(crate) background: ColorPicker,
    pub(crate) cursor: Rc<Cell<CursorShape>>,
    pub(crate) cursor_blink: gtk::Switch,
    pub(crate) features: WorkspaceFeatureFields,
    pub(crate) env_box: gtk::Box,
    pub(crate) env_rows: RefCell<Vec<(gtk::Entry, gtk::Entry)>>,
    pub(crate) mount_box: gtk::Box,
    pub(crate) mount_rows: RefCell<Vec<(gtk::Entry, gtk::Entry, gtk::CheckButton)>>,
}

impl Form {
    pub(crate) fn open(app: &gtk::Application, on_created: Rc<dyn Fn()>) {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("New workspace")
            .default_width(620)
            .default_height(430)
            .modal(false)
            .build();

        let form = Rc::new(build_form());
        use screens::workspace::create::Page as CreatePage;
        let view = screens::workspace::create::View::new([
            (CreatePage::General, form.general()),
            (CreatePage::Terminal, form.terminal()),
            (CreatePage::Resources, form.resources()),
            (CreatePage::Environment, form.environment()),
            (CreatePage::Mounts, form.mounts()),
            (CreatePage::Docker, form.docker()),
            (CreatePage::Network, form.network()),
            (CreatePage::Applications, form.applications()),
            (CreatePage::Compute, form.compute()),
        ]);

        {
            let w = window.clone();
            view.cancel.connect_clicked(move |_| w.close());
        }
        {
            let form = form.clone();
            let w = window.clone();
            let on_created = on_created.clone();
            let pages = view.pages.clone();
            view.create.connect_clicked(move |_| {
                // Validate: name + image are required. Mark empties red and jump to General.
                let name_ok = !form.name.text().trim().is_empty();
                let img_ok = !form.image.text().trim().is_empty();
                form.name.remove_css_class("err");
                form.image.remove_css_class("err");
                if !name_ok || !img_ok {
                    FormValidation::mark_required(&form, name_ok, img_ok);
                    pages.set_visible_child_name("General");
                    FormValidation::focus_missing(&form, name_ok);
                    return;
                }
                if form.save() {
                    on_created();
                    w.close();
                }
            });
        }

        // Debug: HL_TERM_NEWWS_PANE selects a config pane for screenshotting.
        if let Some(p) = AppConfig::get().new_workspace_pane.as_deref() {
            view.select_name(p);
        }

        window.set_child(Some(&view.widget));
        window.present();
        host::appearance::Appearance::apply();
        Screenshot::schedule(&window, "newws");
    }
}

pub(crate) fn build_form() -> Form {
    let terminal = TermConfig::default();
    let font_size = gtk::SpinButton::with_range(6.0, 48.0, 1.0);
    font_size.set_value(terminal.font_size);
    let cursor_blink = gtk::Switch::new();
    cursor_blink.set_active(terminal.cursor_blink);
    Form {
        name: Field::entry("name", false),
        image: Field::entry("ubuntu:24.04", true),
        shell: Field::entry("/bin/bash -l", true),
        storage: Field::entry("", true),
        cpu_amd: Rc::new(Cell::new(false)),
        cpus: gtk::SpinButton::with_range(0.0, 64.0, 1.0),
        mem: gtk::SpinButton::with_range(0.0, 65536.0, 256.0),
        scrollback: Field::entry("unlimited", false),
        font: FontPicker::new(&terminal.font_family),
        font_size,
        foreground: ColorPicker::new(&terminal.foreground),
        background: ColorPicker::new(&terminal.background),
        cursor: Rc::new(Cell::new(terminal.cursor_shape)),
        cursor_blink,
        features: WorkspaceFeatureFields::new(),
        env_box: gtk::Box::new(gtk::Orientation::Vertical, 6),
        env_rows: RefCell::new(Vec::new()),
        mount_box: gtk::Box::new(gtk::Orientation::Vertical, 6),
        mount_rows: RefCell::new(Vec::new()),
    }
}

impl Form {
    pub(crate) fn terminal(&self) -> gtk::Box {
        let panel = Panel::new("Terminal").into_widget();
        self.font.widget().set_hexpand(true);
        panel.append(&Field::labeled("FONT", self.font.widget()));
        panel.append(&Field::spin("FONT SIZE", &self.font_size));
        panel.append(&Field::labeled("BACKGROUND", self.background.widget()));
        panel.append(&Field::labeled("TEXT", self.foreground.widget()));

        let cursor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cursor.add_css_class("seg");
        let block = gtk::ToggleButton::with_label("block");
        let beam = gtk::ToggleButton::with_label("beam");
        let underline = gtk::ToggleButton::with_label("underline");
        beam.set_group(Some(&block));
        underline.set_group(Some(&block));
        match self.cursor.get() {
            CursorShape::Block => block.set_active(true),
            CursorShape::Beam => beam.set_active(true),
            CursorShape::Underline => underline.set_active(true),
        }
        for (button, shape) in [
            (&block, CursorShape::Block),
            (&beam, CursorShape::Beam),
            (&underline, CursorShape::Underline),
        ] {
            ToggleValue::cursor(button, self.cursor.clone(), shape);
            cursor.append(button);
        }
        panel.append(&Field::labeled("CURSOR", &cursor));
        panel.append(&Field::toggle(
            "Cursor blink",
            "Blink the text cursor.",
            &self.cursor_blink,
        ));
        self.scrollback.set_max_width_chars(14);
        self.scrollback.set_halign(gtk::Align::Start);
        panel.append(&Field::text(
            "SCROLLBACK",
            &self.scrollback,
            Some("Blank keeps unlimited history."),
        ));
        panel
    }

    pub(crate) fn general(self: &Rc<Self>) -> gtk::Box {
        let form = self;
        let p = Panel::new("General").into_widget();
        p.append(&Field::text(
            "NAME",
            &form.name,
            Some("A friendly name for this workspace."),
        ));

        // Architecture segmented control (arm64 / x86-64) — built first so the OS control can toggle it.
        let a_seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        a_seg.add_css_class("seg");
        let arm = gtk::ToggleButton::with_label("arm64");
        let amd = gtk::ToggleButton::with_label("x86-64");
        amd.set_group(Some(&arm));
        arm.set_active(true);
        {
            let c = form.cpu_amd.clone();
            arm.connect_toggled(move |t| {
                if t.is_active() {
                    c.set(false);
                }
            });
        }
        {
            let c = form.cpu_amd.clone();
            amd.connect_toggled(move |t| {
                if t.is_active() {
                    c.set(true);
                }
            });
        }
        a_seg.append(&arm);
        a_seg.append(&amd);

        // Only Linux guests are supported; the workspace OS is always Linux.
        p.append(&Field::labeled("ARCHITECTURE", &a_seg));

        // IMAGE comes AFTER OS + ARCH: pick the arch first, then choose from images built for it. The
        // "Choose…" picker reads the current os/arch selection, so it only offers matching templates.
        let irow = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let il = gtk::Label::new(Some("IMAGE"));
        il.add_css_class("flabel");
        il.set_xalign(0.0);
        irow.append(&il);
        let ibox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        form.image.set_hexpand(true);
        let choose = gtk::Button::with_label("Choose…");
        choose.add_css_class("btn");
        ibox.append(&form.image);
        ibox.append(&choose);
        irow.append(&ibox);
        let ih = gtk::Label::new(Some(
            "Pick a template for the selected architecture, or type any Docker image reference.",
        ));
        ih.add_css_class("fhint");
        ih.set_xalign(0.0);
        irow.append(&ih);
        {
            let form2 = form.clone();
            choose.connect_clicked(move |b| {
                if let Some(win) = b.root().and_downcast::<gtk::Window>() {
                    let architecture = if form2.cpu_amd.get() {
                        Arch::Amd64
                    } else {
                        Arch::Arm64
                    };
                    let image = form2.image.clone();
                    ImagePicker::new(architecture).present(&win, move |reference| {
                        image.set_text(reference);
                    });
                }
            });
        }
        p.append(&irow);

        p.append(&Field::text("DEFAULT SHELL", &form.shell, None));

        // Storage location: an entry + a Browse… folder picker.
        let srow = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let sl = gtk::Label::new(Some("STORAGE LOCATION"));
        sl.add_css_class("flabel");
        sl.set_xalign(0.0);
        srow.append(&sl);
        let sbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        form.storage.set_hexpand(true);
        let browse = gtk::Button::with_label("Browse…");
        browse.add_css_class("btn");
        sbox.append(&form.storage);
        sbox.append(&browse);
        srow.append(&sbox);
        let sh = gtk::Label::new(Some(
        "Holds this workspace's docker images, volumes + state. Blank = ~/.hl/workspaces/<name>.",
    ));
        sh.add_css_class("fhint");
        sh.set_xalign(0.0);
        srow.append(&sh);
        {
            let entry = form.storage.clone();
            browse.connect_clicked(move |button| {
                let parent = button.root().and_downcast::<gtk::Window>();
                let entry = entry.clone();
                hl_gui::gtk::DirectoryPicker::new("Choose workspace storage").present(
                    parent.as_ref(),
                    move |path| {
                        entry.set_text(&path.to_string_lossy());
                    },
                );
            });
        }
        p.append(&srow);
        p
    }
}

impl Form {
    pub(crate) fn resources(self: &Rc<Self>) -> gtk::Box {
        let form = self;
        let p = Panel::new("Resources").into_widget();
        form.cpus.set_value(0.0);
        form.mem.set_value(0.0);
        p.append(&Field::spin("CPU CORES (0 = unlimited)", &form.cpus));
        p.append(&Field::spin("MEMORY MB (0 = unlimited)", &form.mem));
        let hint = gtk::Label::new(Some("Caps applied to the workspace's containers."));
        hint.add_css_class("fhint");
        hint.set_xalign(0.0);
        p.append(&hint);
        p
    }

    pub(crate) fn environment(self: &Rc<Self>) -> gtk::Box {
        let form = self;
        let p = Panel::new("Environment").into_widget();
        p.append(&form.env_box);
        let add = gtk::Button::with_label("+ Add variable");
        add.add_css_class("addrow");
        add.set_halign(gtk::Align::Start);
        let form2 = form.clone();
        add.connect_clicked(move |_| form2.add_environment());
        p.append(&add);
        form.add_environment(); // start with one empty row
        p
    }

    pub(crate) fn mounts(self: &Rc<Self>) -> gtk::Box {
        let form = self;
        let p = Panel::new("Mounts").into_widget();
        p.append(&form.mount_box);
        let add = gtk::Button::with_label("+ Add mount");
        add.add_css_class("addrow");
        add.set_halign(gtk::Align::Start);
        let form2 = form.clone();
        add.connect_clicked(move |_| form2.add_mount());
        p.append(&add);
        p
    }
}

impl Form {
    pub(crate) fn docker(&self) -> gtk::Box {
        let pane = Panel::new("Docker").into_widget();
        self.features.docker.set_active(true);
        pane.append(&self.features.docker());
        pane
    }
    pub(crate) fn applications(&self) -> gtk::Box {
        let p = Panel::new("Applications").into_widget();
        p.append(&self.features.applications());
        p
    }

    pub(crate) fn network(&self) -> gtk::Box {
        let p = Panel::new("Network").into_widget();
        p.append(&self.features.network());
        p
    }

    pub(crate) fn compute(&self) -> gtk::Box {
        let p = Panel::new("Compute").into_widget();
        p.append(&self.features.cuda());
        p
    }
}

impl Form {
    pub(crate) fn add_environment(self: &Rc<Self>) {
        let form = self;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let k = Field::entry("KEY", true);
        let v = Field::entry("value", true);
        k.set_hexpand(true);
        v.set_hexpand(true);
        let x = gtk::Button::from_icon_name("user-trash-symbolic");
        x.add_css_class("xbtn");
        x.set_tooltip_text(Some("Remove"));
        row.append(&k);
        row.append(&v);
        row.append(&x);
        form.env_box.append(&row);
        form.env_rows.borrow_mut().push((k.clone(), v.clone()));
        let form2 = form.clone();
        let row2 = row.clone();
        let k2 = k.clone();
        x.connect_clicked(move |_| {
            form2.env_box.remove(&row2);
            form2.env_rows.borrow_mut().retain(|(kk, _)| kk != &k2);
        });
    }

    pub(crate) fn add_mount(self: &Rc<Self>) {
        let form = self;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let host = Field::entry("/host/path", true);
        let cont = Field::entry("/container/path", true);
        host.set_hexpand(true);
        cont.set_hexpand(true);
        let ro = gtk::CheckButton::with_label("ro");
        ro.set_valign(gtk::Align::Center);
        let x = gtk::Button::from_icon_name("user-trash-symbolic");
        x.add_css_class("xbtn");
        x.set_tooltip_text(Some("Remove"));
        row.append(&host);
        row.append(&cont);
        row.append(&ro);
        row.append(&x);
        form.mount_box.append(&row);
        form.mount_rows
            .borrow_mut()
            .push((host.clone(), cont.clone(), ro.clone()));
        let form2 = form.clone();
        let row2 = row.clone();
        let h2 = host.clone();
        x.connect_clicked(move |_| {
            form2.mount_box.remove(&row2);
            form2.mount_rows.borrow_mut().retain(|(hh, _, _)| hh != &h2);
        });
    }

    pub(crate) fn save(&self) -> bool {
        let form = self;
        let name = form.name.text().trim().to_string();
        let image = form.image.text().trim().to_string();
        if name.is_empty() || image.is_empty() {
            return false;
        }
        // Map the arch toggle to the internal target (Linux only).
        let arch = if form.cpu_amd.get() {
            Arch::Amd64
        } else {
            Arch::Arm64
        };
        let mut ws = WorkspaceConfig::new(&name, &image, arch);
        let shell = form.shell.text().trim().to_string();
        if !shell.is_empty() {
            ws.shell = Some(shell);
        }
        let storage = form.storage.text().trim().to_string();
        if !storage.is_empty() {
            ws.storage = Some(std::path::PathBuf::from(storage));
        }
        let c = form.cpus.value() as u32;
        if c > 0 {
            ws.cpus = Some(c);
        }
        let m = form.mem.value() as u32;
        if m > 0 {
            ws.memory_mb = Some(m);
        }
        // Terminal scrollback: blank / 0 / "unlimited" → None (unlimited); a positive number → cap.
        let sb = form.scrollback.text().trim().to_ascii_lowercase();
        ws.scrollback = match sb.as_str() {
            "" | "0" | "unlimited" => None,
            _ => sb.parse::<u64>().ok().filter(|n| *n > 0),
        };
        ws.terminal = TerminalPreferences {
            font_family: Some(form.font.value()),
            font_size: Some(form.font_size.value().round() as u16),
            foreground: Some(form.foreground.value()),
            background: Some(form.background.value()),
            cursor_shape: Some(form.cursor.get().as_str().to_owned()),
            cursor_blink: Some(form.cursor_blink.is_active()),
        };
        ws.docker_sock = form.features.docker.is_active();
        ws.gui = form.features.graphical.is_active();
        // VPN/proxy egress: blank → direct (None); otherwise parse the spec (bare host:port defaults to SOCKS5).
        ws.vpn = VpnConfig::parse(form.features.vpn.text().trim());
        // Simulated CUDA device: off → None; on → build the reported device props (backed by host Metal).
        ws.cuda = if form.features.cuda.is_active() {
            let mut d = CudaDevice::default_device();
            let name = form.features.cuda_name.text().trim().to_string();
            if !name.is_empty() {
                d.name = name;
            }
            let cc = form.features.cuda_capability.text().trim().to_string();
            if !cc.is_empty() {
                d.compute_capability = cc;
            }
            if let Ok(mb) = form.features.cuda_memory.text().trim().parse::<u32>() {
                d.vram_mb = mb.max(1);
            }
            Some(d)
        } else {
            None
        };
        for (k, v) in form.env_rows.borrow().iter() {
            let key = k.text().trim().to_string();
            if !key.is_empty() {
                ws.env.push((key, v.text().trim().to_string()));
            }
        }
        for (h, c, ro) in form.mount_rows.borrow().iter() {
            let host = h.text().trim().to_string();
            let cont = c.text().trim().to_string();
            if !host.is_empty() && !cont.is_empty() {
                ws.mounts.push(Mount {
                    host,
                    container: cont,
                    ro: ro.is_active(),
                });
            }
        }
        let mut store = WorkspaceStore::load(Home::current().workspaces_config());
        store.upsert(ws).is_ok()
    }
}

// ---- new-workspace widget helpers ----
