//! Production Top process → socket protocol → retained tree → GTK screenshots.

#[cfg(unix)]
mod unix {
    use std::io::{self, Read as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use hl_extension::port::{ExtensionCatalogue, ExtensionCatalogueEntry};
    use hl_extension::{
        codec, Capability, ExtensionName, ExtensionPreferences, ExtensionSummary, Frame, Grant, Hello, PreferenceValue,
        Reply, Request, Welcome, Wire, WorkspaceConfiguration, WorkspaceInfo, WorkspaceTerminal, PROTOCOL,
    };
    use hl_gui::{Renderer as _, Theme, Tree};
    use hl_gui_gtk::Surface;

    const CASES: &[(&str, &str)] = &[
        ("workspace", "workspace"),
        ("settings", "settings"),
        ("extensions", "extensions"),
        ("networks", "networks"),
    ];
    const DEADLINE: Duration = Duration::from_secs(5);
    const PATCH_LIMIT: usize = 1_500;

    struct TopChild(Child);

    impl TopChild {
        fn stop(&mut self) -> String {
            if self.0.try_wait().expect("Top process status reads").is_none() {
                self.0.kill().expect("Top test process stops");
            }
            self.0.wait().expect("Top process is reaped");
            let mut stderr = String::new();
            self.0
                .stderr
                .take()
                .expect("captured stderr")
                .read_to_string(&mut stderr)
                .expect("stderr reads");
            stderr
        }
    }

    impl Drop for TopChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    #[test]
    fn production_top_renders_bounded_populated_and_error_fixtures_at_both_widths() {
        assert!(gtk::init().is_ok(), "run this test under Xvfb");
        let repository = repository();
        assert!(
            repository.join("extensions/top/dist/main.js").exists(),
            "run `npm --prefix extensions run build:sdk && npm --prefix extensions run build --workspace @husklet/top`"
        );
        for fixture in ["populated", "error"] {
            for (name, section) in CASES {
                render_case(&repository, fixture, name, section);
            }
        }
    }

    fn render_case(repository: &Path, fixture: &str, name: &str, section: &str) {
        let socket = std::env::temp_dir().join(format!("husklet-top-{}-{fixture}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("Top test socket binds");
        listener.set_nonblocking(true).expect("listener is deadline-bound");
        let mut child = TopChild(
            Command::new("node")
                .arg(repository.join("extensions/top/dist/main.js"))
                .env("HUSKLET_EXTENSION_SOCKET", &socket)
                .env("HUSKLET_TOP_FIXTURE", fixture)
                .env("HUSKLET_TOP_SECTION", section)
                .current_dir(repository.join("extensions/top"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("production Top entrypoint starts"),
        );
        let stream = accept_before(&listener, Instant::now() + DEADLINE)
            .unwrap_or_else(|error| panic!("Top connects: {error}; stderr: {}", child.stop()));
        stream
            .set_read_timeout(Some(Duration::from_millis(40)))
            .expect("Top reads are bounded");
        stream
            .set_write_timeout(Some(DEADLINE))
            .expect("Top writes are bounded");
        let mut wire = Wire::new(stream);
        wire.send(
            &codec::welcome(&Welcome {
                protocol: PROTOCOL,
                host: "top-gtk-e2e".into(),
                workspace: "fixture-workspace".into(),
                peer: ExtensionName::new("top").expect("valid extension name"),
                granted: Grant::new([
                    Capability::Interface,
                    Capability::PreferenceRead,
                    Capability::WorkspaceRead,
                    Capability::ExtensionRead,
                    Capability::ContainerRead,
                    Capability::ImageRead,
                    Capability::VolumeRead,
                    Capability::NetworkRead,
                    Capability::TerminalRead,
                ]),
                limits: hl_extension::Limits::default(),
            })
            .expect("welcome encodes"),
        )
        .expect("welcome sends");
        let hello: Hello = codec::read_hello(&receive_until(&mut wire, Instant::now() + DEADLINE).expect("Top greets"))
            .expect("hello decodes");
        assert_eq!(hello.protocol, PROTOCOL);

        let mut tree = Tree::new();
        let mut surface = Surface::new();
        surface.theme(&Theme::dark()).expect("Top theme installs");
        let mut renders = 0;
        let mut quiet = 0;
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline && (renders == 0 || quiet < 4) {
            match receive_until(&mut wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                Ok(frame) => {
                    quiet = 0;
                    let request = codec::read_request(&frame).expect("Top request decodes");
                    let reply = match request {
                        Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                            assert!(
                                frame.patches.len() <= PATCH_LIMIT,
                                "{fixture}/{name} emitted {} patches",
                                frame.patches.len()
                            );
                            tree.apply(&frame, &mut surface)
                                .unwrap_or_else(|error| panic!("{fixture}/{name} failed in GTK: {error:?}"));
                            renders += 1;
                            Reply::Done
                        }
                        Request::InterfaceOpenTab { .. } => Reply::Identity("top-main".into()),
                        Request::PreferenceRead => Reply::Preferences(ExtensionPreferences {
                            revision: 1,
                            entries: vec![("sidebar-width".into(), PreferenceValue::Number(232))],
                        }),
                        Request::WorkspaceInfo => Reply::Workspace(workspace_info()),
                        Request::WorkspaceInspect { .. } => Reply::WorkspaceConfiguration(workspace_configuration()),
                        Request::ExtensionList => Reply::Extensions(extensions()),
                        Request::ExtensionCatalogue => Reply::ExtensionCatalogue(catalogue()),
                        Request::SourceResize { .. }
                        | Request::SourceResizeAt { .. }
                        | Request::EventSubscribe { .. }
                        | Request::EventUnsubscribe { .. } => Reply::Done,
                        other => panic!("unexpected {fixture}/{name} Top call: {other:?}"),
                    };
                    wire.send(&codec::reply(&reply).expect("reply encodes"))
                        .expect("reply sends");
                }
                Err(hl_extension::Transit::Pending) => quiet += 1,
                Err(error) => panic!("{fixture}/{name} socket failed: {error:?}; stderr: {}", child.stop()),
            }
        }
        assert!(renders >= 2, "{fixture}/{name} never replaced its bootstrap surface");

        let root = surface.widget().clone().upcast::<gtk::Widget>();
        let heading = match name {
            "workspace" => "Workspace",
            "settings" => "Workspace settings",
            "extensions" => "Extensions",
            "networks" => "Networks",
            _ => unreachable!(),
        };
        assert!(has_label(&root, heading), "{fixture}/{name} did not render {heading:?}");
        if fixture == "error" && name == "extensions" {
            assert!(
                has_label(
                    &root,
                    "Extension catalogue lost sync with the extension host. No change was assumed."
                ),
                "the error fixture did not reach its actionable recovery state"
            );
        }
        if fixture == "error" && name == "settings" {
            assert!(
                has_label(&root, "Workspace settings could not be loaded from the extension host."),
                "the settings error fixture exposed an editable form instead of its load failure"
            );
            assert!(has_label(&root, "Retry"), "the settings load failure had no recovery action");
            assert!(!has_label(&root, "Up to date"), "untrusted settings were presented as current");
        }
        let window = gtk::Window::new();
        window.set_child(Some(&root));
        for (width_name, width) in [("narrow", 600), ("wide", 1_200)] {
            window.set_default_size(width, 800);
            window.set_size_request(width, 800);
            window.present();
            settle_toolkit();
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 1_600, -1, None);
            assert_eq!(root.width(), width, "{fixture}/{name} rejected {width}px");
            assert_contained(&root, &format!("{fixture}/{name}/{width_name}"));
            capture(&window, &format!("{fixture}-{name}-{width_name}"), width, 800);
        }
        let stderr = child.stop();
        assert!(stderr.is_empty(), "{fixture}/{name} wrote to stderr: {stderr}");
        std::fs::remove_file(socket).expect("Top test socket is removed");
    }

    fn workspace_info() -> WorkspaceInfo {
        WorkspaceInfo {
            name: "fixture-workspace".into(),
            architecture: "amd64".into(),
            image: "alpine:3.20".into(),
        }
    }

    fn workspace_configuration() -> WorkspaceConfiguration {
        WorkspaceConfiguration {
            generation: "fixture-generation".into(),
            configuration_revision: "fixture-revision".into(),
            name: "fixture-workspace".into(),
            image: "alpine:3.20".into(),
            architecture: "amd64".into(),
            storage: Some("/workspace".into()),
            shell: Some("/bin/sh".into()),
            cpus: Some(4),
            memory_mb: Some(4096),
            environment: vec![("NODE_ENV".into(), "development".into())],
            environment_redacted: false,
            mounts: Vec::new(),
            docker_socket: false,
            scrollback: Some(10_000),
            vpn: None,
            execution_lifetime: "persisted".into(),
            terminal: WorkspaceTerminal::default(),
        }
    }

    fn extensions() -> Vec<ExtensionSummary> {
        ["top", "storybook"]
            .into_iter()
            .map(|name| ExtensionSummary {
                name: name.into(),
                image_digest: format!("sha256:{}", if name == "top" { "a" } else { "b" }.repeat(64)),
                status: "running".into(),
                version: "0.4.0".into(),
                enabled: true,
                pane_providers: Vec::new(),
                granted: Grant::new([Capability::Interface]),
                images: Default::default(),
                containers: Default::default(),
                networks: Default::default(),
                volumes: Default::default(),
                filesystem: Default::default(),
                workspace_environment: Default::default(),
            })
            .collect()
    }

    fn catalogue() -> ExtensionCatalogue {
        ExtensionCatalogue {
            entries: vec![ExtensionCatalogueEntry {
                id: "storybook".into(),
                title: "Storybook".into(),
                description: "Inspect the Husklet interface component library.".into(),
                version: "0.4.0".into(),
                reference: "ghcr.io/husklet/storybook:0.4.0".into(),
                publisher: "Husklet".into(),
                source: "Built in".into(),
                protocol: PROTOCOL,
                architectures: vec!["amd64".into(), "arm64".into()],
            }],
            complete: true,
        }
    }

    fn accept_before(listener: &UnixListener, deadline: Instant) -> io::Result<UnixStream> {
        loop {
            match listener.accept() {
                Ok((stream, _)) => return Ok(stream),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "connection deadline elapsed"));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn receive_until(wire: &mut Wire<UnixStream>, deadline: Instant) -> Result<Frame, hl_extension::Transit> {
        loop {
            match wire.receive_step() {
                Err(hl_extension::Transit::Pending) if Instant::now() < deadline => {}
                result => return result,
            }
        }
    }

    fn settle_toolkit() {
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
    }

    fn has_label(root: &gtk::Widget, wanted: &str) -> bool {
        if root
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text() == wanted)
        {
            return true;
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if has_label(&current, wanted) {
                return true;
            }
        }
        false
    }

    fn assert_contained(parent: &gtk::Widget, case: &str) {
        if parent.is::<gtk::ScrolledWindow>() || parent.width() <= 0 {
            return;
        }
        let mut child = parent.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if !current.is_visible() {
                continue;
            }
            let allocation = current.allocation();
            assert!(
                allocation.x() >= 0 && allocation.x() + allocation.width() <= parent.width(),
                "{case} overflowed {:?}: child x={} width={}, parent width={}",
                (
                    parent.css_classes(),
                    current.css_classes(),
                    current.downcast_ref::<gtk::Label>().map(gtk::Label::text)
                ),
                allocation.x(),
                allocation.width(),
                parent.width()
            );
            assert_contained(&current, case);
        }
    }

    fn capture(window: &gtk::Window, name: &str, width: i32, height: i32) {
        let Some(directory) = std::env::var_os("HUSKLET_TOP_SHOT") else {
            return;
        };
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).expect("Top screenshot directory is created");
        let paintable = gtk::WidgetPaintable::new(Some(window.upcast_ref::<gtk::Widget>()));
        let node = (0..20)
            .find_map(|_| {
                window.queue_draw();
                settle_toolkit();
                let snapshot = gtk::Snapshot::new();
                paintable.snapshot(snapshot.upcast_ref::<gtk::gdk::Snapshot>(), width as f64, height as f64);
                let node = snapshot.to_node();
                if node.is_none() {
                    std::thread::sleep(Duration::from_millis(10));
                    settle_toolkit();
                }
                node
            })
            .expect("Top window produces a render node");
        window
            .renderer()
            .expect("Top window has a renderer")
            .render_texture(&node, None)
            .save_to_png(directory.join(format!("{name}.png")))
            .expect("Top screenshot is written");
    }

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crate lives below repository/src/workspaces")
            .to_path_buf()
    }
}
