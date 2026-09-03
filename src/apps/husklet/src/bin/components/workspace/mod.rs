use super::inputs::{FormValidation, ImagePicker, WorkspaceFeatureFields};
use super::layout::{Field, Panel};
use crate::*;

mod configuration;

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
    pub(crate) cursor_buttons: [gtk::ToggleButton; 3],
    pub(crate) cursor_blink: gtk::Switch,
    pub(crate) features: WorkspaceFeatureFields,
    pub(crate) env_box: gtk::Box,
    pub(crate) env_rows: RefCell<Vec<(gtk::Entry, gtk::Entry)>>,
    pub(crate) mount_box: gtk::Box,
    pub(crate) mount_rows: RefCell<Vec<(gtk::Entry, gtk::Entry, gtk::CheckButton)>>,
}

impl Form {
    pub(crate) fn open(app: &gtk::Application, on_created: &Rc<dyn Fn()>) {
        use screens::workspace::create::Page as CreatePage;

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("New workspace")
            .default_width(620)
            .default_height(430)
            .modal(false)
            .build();

        let form = Rc::new(Form::new());
        form.add_environment();
        let view = screens::workspace::create::View::new([
            (CreatePage::General, form.general()),
            (CreatePage::Terminal, form.terminal()),
            (CreatePage::Resources, form.resources()),
            (CreatePage::Environment, form.environment()),
            (CreatePage::Mounts, form.mounts()),
            (CreatePage::Docker, form.docker()),
            (CreatePage::Network, form.network()),
        ]);
        form.bind_creation_requirements(&view.create);
        window.set_default_widget(Some(&view.create));

        {
            let w = window.clone();
            view.cancel.connect_clicked(move |_| w.close());
        }
        {
            let form = form.clone();
            let w = window.clone();
            let on_created = on_created.clone();
            let pages = view.pages.clone();
            let status = view.status.clone();
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
                let result = form.configuration().and_then(|workspace| {
                    let mut store = WorkspaceStore::load(Home::current().workspaces_config())?;
                    create_workspace(&mut store, workspace)
                });
                match result {
                    Ok(()) => {
                        on_created();
                        w.close();
                    }
                    Err(error) => {
                        status.add_css_class("err");
                        status.set_text(&error.to_string());
                    }
                }
            });
        }

        // Debug: HL_TERM_NEWWS_PANE selects a config pane for screenshotting.
        if let Some(p) = AppConfig::get().new_workspace_pane.as_deref() {
            view.select_name(p);
        }

        window.set_child(Some(&view.widget));
        window.present();
        if AppConfig::get().open_color_picker {
            let picker = form.background.widget().clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                picker.activate();
            });
            let picker = form.background.widget().clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                picker.activate();
            });
        }
        host::appearance::Appearance::apply();
        Screenshot::schedule(&window, "newws");
    }

    /// Keeps the primary creation action truthful while required values are
    /// being edited. Submission validates again because callbacks may still be
    /// invoked programmatically and the rest of the form has richer rules.
    fn bind_creation_requirements(&self, create: &gtk::Button) {
        let update = {
            let name = self.name.clone();
            let image = self.image.clone();
            let create = create.clone();
            move || {
                create.set_sensitive(!name.text().trim().is_empty() && !image.text().trim().is_empty());
            }
        };
        update();
        {
            let update = update.clone();
            self.name.connect_changed(move |_| update());
        }
        self.image.connect_changed(move |_| update());
    }
}

fn create_workspace(store: &mut WorkspaceStore, workspace: WorkspaceConfig) -> std::io::Result<()> {
    if store.get(&workspace.name).is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("A workspace named {:?} already exists.", workspace.name),
        ));
    }
    store.upsert(workspace)
}

const fn native_workspace_architecture() -> Arch {
    #[cfg(target_arch = "x86_64")]
    {
        Arch::Amd64
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Arch::Arm64
    }
}

impl Form {
    pub(crate) fn new() -> Self {
        let terminal = TermConfig::default();
        let scrollback = terminal
            .scrollback
            .map_or_else(|| "unlimited".to_owned(), |lines| lines.to_string());
        let font_size = gtk::SpinButton::with_range(6.0, 48.0, 1.0);
        font_size.set_value(terminal.font_size);
        let cursor_blink = gtk::Switch::new();
        cursor_blink.set_active(terminal.cursor_blink);
        let cursor = Rc::new(Cell::new(terminal.cursor_shape));
        let cursor_buttons = [
            gtk::ToggleButton::with_label("block"),
            gtk::ToggleButton::with_label("beam"),
            gtk::ToggleButton::with_label("underline"),
        ];
        cursor_buttons[1].set_group(Some(&cursor_buttons[0]));
        cursor_buttons[2].set_group(Some(&cursor_buttons[0]));
        for (button, shape) in cursor_buttons.iter().zip([
            CursorShape::Block,
            CursorShape::Beam,
            CursorShape::Underline,
        ]) {
            button.set_active(terminal.cursor_shape == shape);
            ToggleValue::cursor(button, cursor.clone(), shape);
        }
        Self {
            name: Field::entry("name", false),
            image: Field::entry("ubuntu:24.04", true),
            shell: Field::entry("/bin/bash -l", true),
            storage: Field::entry("", true),
            cpu_amd: Rc::new(Cell::new(native_workspace_architecture() == Arch::Amd64)),
            cpus: gtk::SpinButton::with_range(0.0, 64.0, 1.0),
            mem: gtk::SpinButton::with_range(0.0, 65536.0, 256.0),
            scrollback: Field::entry(&scrollback, false),
            font: FontPicker::new(&terminal.font_family),
            font_size,
            foreground: ColorPicker::new(&terminal.foreground),
            background: ColorPicker::new(&terminal.background),
            cursor,
            cursor_buttons,
            cursor_blink,
            features: WorkspaceFeatureFields::new(),
            env_box: gtk::Box::new(gtk::Orientation::Vertical, 6),
            env_rows: RefCell::new(Vec::new()),
            mount_box: gtk::Box::new(gtk::Orientation::Vertical, 6),
            mount_rows: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn terminal(&self) -> gtk::Box {
        let panel = Panel::new("Terminal").into_widget();
        self.font.widget().set_hexpand(true);
        panel.append(&Field::labeled("FONT", self.font.widget()));
        panel.append(&Field::spin("FONT SIZE", &self.font_size));
        panel.append(&Field::labeled("BACKGROUND", self.background.widget()));
        panel.append(&Field::labeled("TEXT", self.foreground.widget()));

        let cursor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cursor.add_css_class("seg");
        for button in &self.cursor_buttons {
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
            Some("Enter a line limit, or “unlimited”."),
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
        arm.set_active(!form.cpu_amd.get());
        amd.set_active(form.cpu_amd.get());
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
                    let architecture = if form2.cpu_amd.get() { Arch::Amd64 } else { Arch::Arm64 };
                    let image = form2.image.clone();
                    ImagePicker::new(architecture).present(&win, move |reference| image.set_text(reference));
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
                crate::gtk_adapter::DirectoryPicker::new("Choose workspace storage").present(
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
    pub(crate) fn network(&self) -> gtk::Box {
        let p = Panel::new("Network").into_widget();
        p.append(&self.features.network());
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
}

// ---- new-workspace widget helpers ----

#[cfg(test)]
mod create_tests {
    use super::*;

    #[test]
    fn new_workspaces_default_to_the_build_hosts_architecture() {
        #[cfg(target_arch = "x86_64")]
        assert_eq!(native_workspace_architecture(), Arch::Amd64);
        #[cfg(target_arch = "aarch64")]
        assert_eq!(native_workspace_architecture(), Arch::Arm64);
    }

    #[test]
    fn architecture_control_and_persisted_configuration_share_the_native_default() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let form = Rc::new(Form::new());
            let general = form.general();
            #[cfg(target_arch = "x86_64")]
            let expected = Arch::Amd64;
            #[cfg(not(target_arch = "x86_64"))]
            let expected = Arch::Arm64;
            assert_eq!(form.cpu_amd.get(), expected == Arch::Amd64);

            let mut active = None;
            let mut pending = vec![general.upcast::<gtk::Widget>()];
            while let Some(widget) = pending.pop() {
                if let Ok(toggle) = widget.clone().downcast::<gtk::ToggleButton>() {
                    if toggle.is_active() && matches!(toggle.label().as_deref(), Some("arm64" | "x86-64")) {
                        active = toggle.label().map(|label| label.to_string());
                    }
                }
                let mut child = widget.first_child();
                while let Some(current) = child {
                    child = current.next_sibling();
                    pending.push(current);
                }
            }
            assert_eq!(
                active.as_deref(),
                Some(if expected == Arch::Amd64 { "x86-64" } else { "arm64" })
            );

            form.name.set_text("native-default");
            form.image.set_text("ubuntu:24.04");
            assert_eq!(form.configuration().unwrap().arch, expected);
        });
        if !ran {
            eprintln!("skipped: no display connection, so the workspace architecture control cannot be rendered");
        }
    }

    #[test]
    fn duplicate_creation_preserves_the_existing_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workspaces.conf");
        let mut store = WorkspaceStore::load(&path).unwrap();
        store
            .upsert(WorkspaceConfig::new("demo", "original:latest", Arch::Arm64))
            .unwrap();

        let error = create_workspace(
            &mut store,
            WorkspaceConfig::new("demo", "replacement:latest", Arch::Amd64),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

        let reloaded = WorkspaceStore::load(path).unwrap();
        let workspace = reloaded.get("demo").unwrap();
        assert_eq!(workspace.image, "original:latest");
        assert_eq!(workspace.arch, Arch::Arm64);
    }

    #[test]
    fn unique_creation_is_persisted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workspaces.conf");
        let mut store = WorkspaceStore::load(&path).unwrap();

        create_workspace(&mut store, WorkspaceConfig::new("demo", "image:latest", Arch::Arm64)).unwrap();

        assert!(WorkspaceStore::load(path).unwrap().get("demo").is_some());
    }

    #[test]
    fn creation_action_tracks_both_required_fields() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let form = Form::new();
            let create = gtk::Button::with_label("Create workspace");
            form.bind_creation_requirements(&create);

            assert!(!create.is_sensitive());
            form.name.set_text("demo");
            assert!(!create.is_sensitive());
            form.image.set_text("alpine:3.20");
            assert!(create.is_sensitive());
            form.name.set_text("   ");
            assert!(!create.is_sensitive());
        });
        if !ran {
            eprintln!("skipped: no display connection, so creation sensitivity cannot be rendered");
        }
    }
}
