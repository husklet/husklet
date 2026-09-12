//! Production Top process → socket protocol → retained tree → GTK screenshots.

#[cfg(unix)]
mod unix {
    use std::cell::Cell;
    use std::io::{self, Read as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use hl_extension::port::{
        ExecutionSummary, ExtensionAcquisitionJob, ExtensionAcquisitionProgress, ExtensionAcquisitionStatus,
        ExtensionCandidate, ExtensionCatalogue, ExtensionCatalogueEntry, ImageDetails, NetworkEndpointInventory,
        NetworkInventory, NetworkKind, NetworkSummary,
    };
    use hl_extension::{
        Capability, ChannelId, ExtensionName, ExtensionPreferences, ExtensionSummary, FilesystemGrant,
        FilesystemSelector, Frame, Grant, Hello, ImageGrant, ImageSelector, PROTOCOL, PaneProvider, PreferenceValue,
        RelativePath, Reply, Request, Snapshot, VolumeGrant, Welcome, Wire, WorkspaceConfiguration,
        WorkspaceEnvironmentGrant, WorkspaceEnvironmentSelector, WorkspaceInfo, WorkspaceTerminal, codec,
    };
    use hl_gui::{Renderer as _, SourceMutation, Theme, Tree};
    use hl_gui_gtk::Surface;

    const CASES: &[(&str, &str)] = &[
        ("workspace", "workspace"),
        ("settings", "settings"),
        ("extensions", "extensions"),
        ("processes", "processes"),
        ("executions", "executions"),
        ("images", "images"),
        ("volumes", "volumes"),
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
                render_case(&repository, fixture, name, section, false);
            }
        }
        render_case(
            repository.as_path(),
            "partial-processes",
            "processes",
            "processes",
            false,
        );
        render_case(&repository, "populated", "extensions", "extensions", true);
    }

    fn render_case(repository: &Path, fixture: &str, name: &str, section: &str, catalogue_empty: bool) {
        let capture_fixture = if catalogue_empty { "installed" } else { fixture };
        let socket = std::env::temp_dir().join(format!(
            "husklet-top-{}-{capture_fixture}-{name}.sock",
            std::process::id()
        ));
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
                    Capability::ExtensionInstall,
                    Capability::ContainerRead,
                    Capability::ImageRead,
                    if name == "volumes" {
                        Capability::Interface
                    } else {
                        Capability::VolumeRead
                    },
                    Capability::NetworkRead,
                    Capability::NetworkWrite,
                    Capability::TerminalRead,
                ]),
                filesystem: hl_extension::FilesystemGrant::default(),
                containers: hl_extension::ContainerGrant::default(), images: hl_extension::ImageGrant::default(),
                networks: hl_extension::NetworkGrant::default(), volumes: hl_extension::VolumeGrant::default(),
                workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
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
        let mut pending_lengths = Vec::new();
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
                            pending_lengths
                                .retain(|(source, version, rows)| surface.resize(*source, *version, *rows).is_err());
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
                        Request::ExtensionCatalogue => Reply::ExtensionCatalogue(if catalogue_empty {
                            ExtensionCatalogue {
                                entries: Vec::new(),
                                complete: true,
                            }
                        } else {
                            catalogue()
                        }),
                        Request::SourceResize { mutation } | Request::SourceResizeAt { mutation, .. } => {
                            match mutation {
                                SourceMutation::Length { source, version, rows } => {
                                    if surface.resize(source, version, rows).is_err() {
                                        pending_lengths.push((source, version, rows));
                                    }
                                }
                                SourceMutation::Window(window) => {
                                    surface.rows(&window).expect("Top source window applies");
                                }
                                SourceMutation::Invalidate { .. }
                                | SourceMutation::Open { .. }
                                | SourceMutation::Close { .. } => {}
                                _ => {}
                            }
                            Reply::Done
                        }
                        Request::EventSubscribe { .. } | Request::EventUnsubscribe { .. } => Reply::Done,
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
            "processes" => "Processes",
            "executions" => "Executions",
            "images" => "Images",
            "volumes" => "Volumes",
            "networks" => "Networks",
            _ => unreachable!(),
        };
        assert!(has_label(&root, heading), "{fixture}/{name} did not render {heading:?}");
        if fixture == "error" && name == "extensions" {
            assert!(
                !has_label(
                    &root,
                    "Extension catalogue lost sync with the extension host. No change was assumed."
                ),
                "the Installed mode leaked the Discover catalogue failure"
            );
        }
        if fixture == "error" && name == "settings" {
            assert!(
                has_label(&root, "Workspace settings could not be completed."),
                "the settings error fixture exposed an editable form instead of its load failure"
            );
            assert!(
                has_label(&root, "Technical details"),
                "the settings failure hid its diagnostics disclosure"
            );
            assert!(
                has_label(&root, "Retry"),
                "the settings load failure had no recovery action"
            );
            assert!(
                !has_label(&root, "Up to date"),
                "untrusted settings were presented as current"
            );
        }
        if fixture == "error" && name == "networks" {
            assert!(has_label(
                &root,
                "Network inventory is unavailable. Check that the workspace is running, then retry."
            ));
            assert!(has_label(&root, "Retry networks"));
            assert!(has_label(&root, "Technical details"));
            assert!(!has_label(&root, "This view could not be completed."));
            assert!(!find_expander(&root, "Technical details").is_expanded());
            assert_inline_message(
                &root,
                "Network inventory is unavailable. Check that the workspace is running, then retry.",
            );
        }
        if fixture == "error" && name == "workspace" {
            assert!(has_label(
                &root,
                "Workspace inventory lost its connection. No change was assumed."
            ));
            assert!(has_label(&root, "Retry inventory"));
            assert!(has_label(&root, "Technical details"));
            assert!(!find_expander(&root, "Technical details").is_expanded());
            assert_inline_message(&root, "Workspace inventory lost its connection. No change was assumed.");
        }
        if fixture == "populated" && name == "extensions" {
            assert!(
                has_placeholder(&root, "Search installed"),
                "installed management omitted its search control"
            );
            assert!(
                has_label(&root, "50 of 50 installed extensions"),
                "installed management omitted its bounded result count"
            );
            assert!(
                has_label(&root, "Showing 12 of 50 matching installed extensions"),
                "installed management materialized an unbounded card wall"
            );
        }
        let window = gtk::Window::new();
        window.set_child(Some(&root));
        // The fixture's initial catalogue capture leaves this window wide. Keep
        // each subsequent state wide-first so GTK never treats a prior 1200px
        // allocation as the minimum for an attempted narrow allocation.
        for (width_name, width) in [("wide", 1_200), ("narrow", 600)] {
            if name == "volumes" {
                continue;
            }
            window.set_default_size(width, 800);
            window.set_size_request(width, 800);
            window.present();
            settle_toolkit();
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 1_600, -1, None);
            window.queue_draw();
            settle_frame();
            assert_eq!(root.width(), width, "{fixture}/{name} rejected {width}px");
            assert_contained(&root, &format!("{fixture}/{name}/{width_name}"));
            if width == 600 {
                let section = find_label(&root, "Section");
                let bounds = section
                    .compute_bounds(&root)
                    .expect("compact Section label belongs to the rendered root");
                assert!(
                    bounds.x() >= 0.0 && bounds.x() + bounds.width() <= width as f32,
                    "{fixture}/{name} clipped the compact Section label at {bounds:?}"
                );
                let chooser = find_combobox(&root);
                assert!(chooser.grab_focus(), "compact section chooser is keyboard reachable");
            }
            if fixture == "populated" && name == "processes" && width == 1_200 {
                let refresh = find_tooltip_button(&root, "Refresh processes");
                assert_eq!(
                    (refresh.width(), refresh.height()),
                    (28, 28),
                    "process refresh must remain a compact toolbar icon action"
                );
                assert!(refresh.has_css_class("hl-iconbutton"));
                assert!(refresh.has_css_class("size-small"));
                let icon = refresh
                    .child()
                    .and_then(|child| child.downcast::<gtk::Image>().ok())
                    .expect("process refresh renders its native icon");
                assert_eq!(icon.icon_name().as_deref(), Some("view-refresh-symbolic"));
            }
            if fixture == "populated" && name == "settings" {
                let label = find_label(&root, "Execution lifetime");
                let select = label
                    .mnemonic_widget()
                    .filter(|widget| widget.accessible_role() == gtk::AccessibleRole::ComboBox)
                    .expect("Execution lifetime FormLabel names its native Select");
                assert!(
                    select.is_focusable(),
                    "labelled settings Select remains keyboard reachable"
                );
                assert!(
                    select.allocation().height() <= 36,
                    "{width_name} settings Select exceeded the compact 36px control height: {}",
                    select.allocation().height()
                );
                let label_bounds = label
                    .compute_bounds(&root)
                    .expect("settings label belongs to the rendered root");
                let select_bounds = select
                    .compute_bounds(&root)
                    .expect("settings Select belongs to the rendered root");
                let gap = (select_bounds.y() - label_bounds.y() - label_bounds.height()).round() as i32;
                assert!(
                    (0..=8).contains(&gap),
                    "{width_name} settings label gap was {gap}px instead of at most 8px"
                );
                let image = find_entry_placeholder(&root, "registry/image:tag");
                let shell = find_entry_placeholder(&root, "Automatic when empty");
                assert_eq!(
                    find_label(&root, "Workspace image").mnemonic_widget(),
                    Some(image.clone().upcast()),
                    "Workspace image visibly and accessibly labels its Entry"
                );
                assert_eq!(
                    find_label(&root, "Default shell").mnemonic_widget(),
                    Some(shell.clone().upcast()),
                    "Default shell visibly and accessibly labels its Entry"
                );
                let controls = [
                    image.clone().upcast::<gtk::Widget>(),
                    shell.clone().upcast(),
                    select.clone(),
                ];
                let widths = controls.iter().map(gtk::Widget::width).collect::<Vec<_>>();
                assert!(
                    widths.iter().all(|control_width| (440..=500).contains(control_width)),
                    "{width_name} settings controls escaped the compact readable measure: {widths:?}"
                );
                assert_eq!(image.accessible_role(), gtk::AccessibleRole::TextBox);
                assert_eq!(shell.accessible_role(), gtk::AccessibleRole::TextBox);
                assert_eq!(select.accessible_role(), gtk::AccessibleRole::ComboBox);
                if width == 1_200 {
                    assert!(
                        widths.iter().all(|control_width| *control_width < width / 2),
                        "desktop settings controls must not stretch across the entire page: {widths:?}"
                    );
                }
            }
            if fixture == "populated" && name == "images" {
                let reference = find_entry_placeholder(&root, "registry/image:tag");
                let pull = find_button(&root, "Pull");
                let refresh = find_tooltip_button(&root, "Refresh images");
                let reference_bounds = reference
                    .compute_bounds(&root)
                    .expect("image reference belongs to the rendered root");
                let pull_bounds = pull
                    .compute_bounds(&root)
                    .expect("image pull belongs to the rendered root");
                let refresh_bounds = refresh
                    .compute_bounds(&root)
                    .expect("image refresh belongs to the rendered root");
                for (label, bounds) in [("Pull", pull_bounds), ("Refresh", refresh_bounds)] {
                    assert!(
                        (bounds.y() - reference_bounds.y()).abs() <= 2.0,
                        "{width_name} image {label} detached from its field row: field={reference_bounds:?}, action={bounds:?}"
                    );
                }
                assert!(
                    refresh.height() <= 38,
                    "{width_name} image refresh exceeded the compact medium tier: {}px",
                    refresh.height()
                );
                assert!(
                    has_label(&root, "Use a registry reference such as alpine:3.20."),
                    "{width_name} image field keeps concise format guidance"
                );
                let guidance = find_label(&root, "Use a registry reference such as alpine:3.20.");
                let guidance_bounds = guidance
                    .compute_bounds(&root)
                    .expect("image guidance belongs to the rendered root");
                assert!(
                    guidance_bounds.y() - reference_bounds.y() - reference_bounds.height() <= 20.0,
                    "{width_name} image FormControl stretched between its field and guidance: field={reference_bounds:?}, guidance={guidance_bounds:?}"
                );
            }
            if fixture == "populated" && name == "extensions" {
                let cards = widgets_with_class(&root, "hl-card");
                if width == 600 {
                    assert!(
                        cards
                            .windows(2)
                            .all(|pair| pair[0].allocation().y() != pair[1].allocation().y()),
                        "600px Installed cards did not form one full-width row each"
                    );
                } else {
                    let first_y = cards.first().expect("Installed renders cards").allocation().y();
                    assert_eq!(
                        cards.iter().take_while(|card| card.allocation().y() == first_y).count(),
                        3,
                        "1200px Installed collection did not retain three columns"
                    );
                }
                let refresh = find_tooltip_button(&root, "Refresh installed extensions");
                assert_eq!(refresh.icon_name().as_deref(), Some("view-refresh-symbolic"));
                assert!(refresh.has_css_class("size-small"));
                assert_eq!(refresh.height(), 28, "{width_name} refresh uses the compact tier");
                let refresh_bounds = refresh
                    .compute_bounds(&root)
                    .expect("installed refresh belongs to Top root");
                let toolbar = refresh.parent().expect("refresh remains in the installed toolbar");
                let toolbar_bounds = toolbar
                    .compute_bounds(&root)
                    .expect("installed toolbar belongs to Top root");
                assert!(
                    toolbar_bounds.width() >= if width == 600 { 550.0 } else { 600.0 },
                    "{width_name} installed toolbar collapsed around its labels: {toolbar_bounds:?}"
                );
                assert!(
                    (refresh_bounds.x() + refresh_bounds.width()
                        - toolbar_bounds.x()
                        - toolbar_bounds.width())
                        .abs()
                        <= 1.0,
                    "{width_name} refresh is stranded beside the count instead of anchoring the toolbar: refresh={refresh_bounds:?}, toolbar={toolbar_bounds:?}"
                );
            }
            if fixture == "error" && name == "networks" {
                for label in [
                    "Network inventory is unavailable. Check that the workspace is running, then retry.",
                    "Retry networks",
                    "Technical details",
                ] {
                    assert!(
                        vertical_end(&root, &find_labelled(&root, label)) <= 320,
                        "{width_name} network recovery {label:?} fell below the first 320px"
                    );
                }
            }
            if fixture == "populated" && name == "networks" {
                let title = find_heading(&root, "Networks");
                assert!(title.has_css_class("scale-display"));
                assert_eq!(title.accessible_role(), gtk::AccessibleRole::Heading);
                let entry = find_entry_placeholder(&root, "Network name");
                let create = find_button(&root, "Create");
                let refresh = find_tooltip_button(&root, "Refresh networks");
                let manage = find_button(&root, "Manage connections");
                assert_eq!(refresh.icon_name().as_deref(), Some("view-refresh-symbolic"));
                assert_eq!(refresh.accessible_role(), gtk::AccessibleRole::Button);
                assert!(manage.has_css_class("size-small"));
                assert!(
                    manage.allocation().height() <= 40,
                    "{width_name} network management action is too tall: {}px",
                    manage.allocation().height()
                );
                assert!(manage.grab_focus());
                let widgets = [
                    entry.clone().upcast::<gtk::Widget>(),
                    create.clone().upcast(),
                    refresh.clone().upcast(),
                ];
                let tops = widgets.iter().map(|widget| widget.allocation().y()).collect::<Vec<_>>();
                assert!(
                    tops.iter().max().unwrap() - tops.iter().min().unwrap() <= 4,
                    "{width_name} network creation controls do not share a row: {tops:?}"
                );
                let heights = widgets
                    .iter()
                    .map(|widget| widget.allocation().height())
                    .collect::<Vec<_>>();
                assert!(
                    heights.iter().max().unwrap() - heights.iter().min().unwrap() <= 8,
                    "{width_name} network creation controls have mismatched heights: {heights:?}"
                );
                assert!(entry.allocation().x() < create.allocation().x());
                assert!(create.allocation().x() < refresh.allocation().x());
                assert!(vertical_end(&root, refresh.upcast_ref()) <= 240);
                if width == 1_200 {
                    assert_labels_painted(
                        &window,
                        &root,
                        &[
                            "Workspace",
                            "Settings",
                            "Extensions",
                            "Containers",
                            "Processes",
                            "Executions",
                            "Images",
                            "Volumes",
                            "Networks",
                            "Terminals",
                        ],
                    );
                }
            }
            if fixture == "populated" && name == "images" {
                let card = widgets_with_class(&root, "hl-card")
                    .into_iter()
                    .next()
                    .expect("image inventory renders a card");
                let minimum = if width == 1_200 { 900 } else { 540 };
                assert!(
                    card.width() >= minimum,
                    "{width_name} image card collapsed to {}px instead of using the page width",
                    card.width()
                );
                assert!(
                    card.height() <= 128,
                    "{width_name} collapsed execution record is too tall: {}px",
                    card.height()
                );
                assert!(
                    card.height() <= 128,
                    "{width_name} one-line image record is too tall: {}px",
                    card.height()
                );
                let inspect = find_button(&card, "Inspect");
                assert!(inspect.has_css_class("size-small"));
                assert!(inspect.has_css_class("variant-outline"));
                assert!(
                    inspect.height() <= 32,
                    "{width_name} image Inspect action exceeded 32px: {}",
                    inspect.height()
                );
                assert!(inspect.grab_focus(), "image Inspect action is keyboard reachable");
            }
            if fixture == "populated" && name == "executions" {
                let card = widgets_with_class(&root, "hl-card")
                    .into_iter()
                    .next()
                    .expect("execution inventory renders a card");
                let minimum = if width == 1_200 { 900 } else { 540 };
                assert!(
                    card.width() >= minimum,
                    "{width_name} execution card collapsed to {}px instead of using the page width",
                    card.width()
                );
                for label in ["Details", "Load output", "Wait up to 5s"] {
                    let action = find_button(&card, label);
                    assert!(action.has_css_class("size-small"));
                    assert!(
                        action.height() <= 32,
                        "{width_name} execution action {label:?} exceeded 32px: {}",
                        action.height()
                    );
                }
                assert!(
                    find_button(&card, "Details").grab_focus(),
                    "execution Details action is keyboard reachable"
                );
                assert!(
                    find_button(&card, "Load output").grab_focus(),
                    "execution output action is keyboard reachable"
                );
            }
            if fixture != "error" && name == "processes" && width == 1_200 {
                settle_toolkit();
                let request = surface
                    .requests(1)
                    .into_iter()
                    .last()
                    .expect("realized Processes table requests a bounded row window");
                assert!(request.range.count <= 128);
                let channel = ChannelId::new(88);
                let mut request_payload = serde_json::to_value(&request).expect("process row request encodes");
                request_payload
                    .as_object_mut()
                    .expect("row request is an object")
                    .insert("slot".into(), serde_json::Value::String("top-main".into()));
                wire.send(&Frame::new(
                    channel,
                    hl_extension::Kind::Event,
                    serde_json::to_vec(&request_payload).expect("slotted process row request encodes"),
                ))
                .expect("process row request reaches Top");
                loop {
                    let frame = receive_until(&mut wire, Instant::now() + DEADLINE)
                        .expect("Top publishes its process row window");
                    if frame.kind == hl_extension::Kind::Credit {
                        continue;
                    }
                    if frame.kind == hl_extension::Kind::Response {
                        assert_eq!(frame.channel, channel, "Top answers the exact row channel");
                        let window: hl_gui::RowWindow =
                            serde_json::from_slice(&frame.payload).expect("process row window decodes");
                        assert_eq!(window.source, request.source);
                        assert_eq!(window.version, request.version);
                        assert_eq!(window.request, request.id);
                        assert_eq!(window.range, request.range);
                        surface.rows(&window).expect("GTK accepts process rows");
                        break;
                    }
                    let concurrent = codec::read_request(&frame).expect("concurrent Top call decodes");
                    let reply = match concurrent {
                        Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                            tree.apply(&frame, &mut surface).expect("process rerender applies");
                            Reply::Done
                        }
                        Request::SourceResize { mutation } | Request::SourceResizeAt { mutation, .. } => {
                            assert!(
                                !matches!(mutation, SourceMutation::Window(_)),
                                "Top must answer row requests on their correlated response channel"
                            );
                            Reply::Done
                        }
                        other => panic!("unexpected call awaiting process rows: {other:?}"),
                    };
                    wire.send(&codec::reply(&reply).expect("concurrent reply encodes"))
                        .expect("concurrent reply sends");
                }
                settle_toolkit();
            }
            if fixture == "populated" && name == "workspace" && width == 1_200 {
                let navigation_headers = widgets_with_class(&root, "hl-listsubheader");
                assert_eq!(
                    navigation_headers.len(),
                    4,
                    "Top renders one semantic heading for each navigation group"
                );
                assert_eq!(
                    navigation_headers
                        .iter()
                        .filter_map(|header| header.downcast_ref::<gtk::Label>())
                        .map(|header| header.text().to_string())
                        .collect::<Vec<_>>(),
                    ["Manage", "Runtime", "Resources", "Interface"]
                );
                for header in &navigation_headers {
                    assert_eq!(header.accessible_role(), gtk::AccessibleRole::Heading);
                    assert_eq!(header.height(), 16, "Top group headings remain compact");
                    let description = header
                        .pango_context()
                        .font_description()
                        .expect("Top group heading has computed typography");
                    assert_eq!(description.size(), 11 * gtk::pango::SCALE);
                    assert_eq!(description.weight(), gtk::pango::Weight::Semibold);
                    let bounds = header
                        .compute_bounds(&root)
                        .expect("Top group heading belongs to the desktop rail");
                    assert!(
                        bounds.x() >= 4.0 && bounds.x() + bounds.width() <= 156.0,
                        "Top group heading escaped the 4px inset of the 160px rail: {bounds:?}"
                    );
                }
                let destinations = widgets_with_class(&root, "hl-navigationmenuitem");
                assert_eq!(destinations.len(), 10, "Top renders every desktop destination once");
                let heights = destinations.iter().map(gtk::Widget::height).collect::<Vec<_>>();
                assert!(
                    heights.iter().all(|height| *height == 28),
                    "Top navigation rows were not exactly 28px: {heights:?}"
                );
                for destination in &destinations {
                    assert_eq!(
                        destination.accessible_role(),
                        gtk::AccessibleRole::ToggleButton,
                        "selected destinations expose native toggle semantics"
                    );
                    assert!(
                        destination.is_focusable(),
                        "every Top destination is keyboard reachable"
                    );
                    let bounds = destination
                        .compute_bounds(&root)
                        .expect("desktop destination belongs to the Top root");
                    assert!(
                        bounds.x() >= 4.0 && bounds.x() + bounds.width() <= 156.0,
                        "desktop destination escaped the 4px inset of the 160px rail: {bounds:?}"
                    );
                }
                let selected = destinations
                    .iter()
                    .find(|destination| has_label(destination, "Workspace"))
                    .and_then(|destination| destination.downcast_ref::<gtk::ToggleButton>())
                    .expect("Workspace is the selected desktop destination");
                let hovered = destinations
                    .iter()
                    .find(|destination| has_label(destination, "Settings"))
                    .and_then(|destination| destination.downcast_ref::<gtk::ToggleButton>())
                    .expect("Settings is the adjacent desktop destination");
                assert!(selected.is_active());
                assert!(selected.has_css_class("variant-filled"));
                assert!(selected.has_css_class("tone-accent"));
                assert!(!hovered.is_active());
                assert!(hovered.has_css_class("variant-ghost"));
                assert!(hovered.has_css_class("tone-neutral"));
                for (label, icon) in [
                    ("Settings", "emblem-system-symbolic"),
                    ("Terminals", "application-x-executable-symbolic"),
                ] {
                    let destination = destinations
                        .iter()
                        .find(|destination| has_label(destination, label))
                        .expect("semantic Top destination exists");
                    let emblem = widgets_with_class(destination, "hl-emblem")
                        .into_iter()
                        .next()
                        .and_then(|widget| widget.downcast::<gtk::Image>().ok())
                        .expect("Top destination retains an emblem");
                    assert_eq!(
                        emblem.icon_name().as_deref(),
                        Some(icon),
                        "{label} must not collapse to the generic more-actions icon"
                    );
                }
                assert!(selected.grab_focus(), "selected Top destination accepts keyboard focus");
                hovered.set_state_flags(gtk::StateFlags::PRELIGHT, false);
                settle_toolkit();
                assert!(
                    selected.has_focus(),
                    "selected Top destination owns the native focus state"
                );
                assert!(
                    hovered.state_flags().contains(gtk::StateFlags::PRELIGHT),
                    "hovered Top destination exposes the native prelight state"
                );
                let paned = widgets_with_class(&root, "hl-responsive-divider")
                    .into_iter()
                    .next()
                    .and_then(|widget| widget.downcast::<gtk::Paned>().ok())
                    .expect("Top desktop navigation owns a responsive divider");
                assert!(
                    paned.vexpands(),
                    "Top desktop divider must expand through the available pane height"
                );
                assert_eq!(paned.accessible_role(), gtk::AccessibleRole::Separator);
                assert!(paned.is_focusable(), "Top divider is keyboard reachable");
                let navigation = paned.start_child().expect("Top divider retains navigation");
                let body = paned.end_child().expect("Top divider retains the selected section");
                let navigation_bounds = navigation
                    .compute_bounds(&paned)
                    .expect("Top navigation belongs to its divider");
                let body_bounds = body.compute_bounds(&paned).expect("Top section belongs to its divider");
                assert_eq!(
                    (body_bounds.x() - navigation_bounds.x() - navigation_bounds.width()).round(),
                    8.0,
                    "Top responsive divider must expose an exact 8px interaction target"
                );
                let body_before = body.width();
                paned.set_position(200);
                root.allocate(1_200, 1_600, -1, None);
                settle_toolkit();
                assert_eq!(navigation.width(), 200, "native divider resizes the Top rail");
                assert_eq!(
                    body.width(),
                    body_before - 40,
                    "resizing the Top rail transfers exactly the same width from its content"
                );
                assert!(paned.grab_focus(), "resized Top divider accepts keyboard focus");
                assert!(
                    paned.has_focus(),
                    "resized Top divider exposes its focused handle state"
                );
                assert_overview_grid(&root, 1_200, 224, "resized-wide");
                capture(&window, "populated-workspace-resized-wide", 1_200, 800);
                paned.set_position(160);
                root.allocate(1_200, 1_600, -1, None);
                selected.grab_focus();
                settle_toolkit();
            }
            if fixture == "populated" && name == "workspace" {
                let content_start = if width == 600 { 16 } else { 184 };
                assert_overview_grid(&root, width, content_start, width_name);
            }
            if fixture == "error" && name == "workspace" {
                assert_overview_recovery(&root, width, width_name);
            }
            capture(&window, &format!("{capture_fixture}-{name}-{width_name}"), width, 800);
            if fixture == "error" && name == "workspace" {
                let disclosure = find_expander(&root, "Technical details");
                assert!(
                    disclosure.grab_focus(),
                    "{width_name} recovery disclosure accepts focus"
                );
                disclosure.emit_by_name::<()>("activate", &[]);
                settle_toolkit();
                assert!(disclosure.is_expanded(), "{width_name} recovery disclosure opens");
                find_mapped_labelled(&root, "workspace daemon socket refused the connection");
                capture(&window, &format!("error-workspace-details-{width_name}"), width, 800);
                disclosure.emit_by_name::<()>("activate", &[]);
                settle_toolkit();
                assert!(!disclosure.is_expanded(), "{width_name} recovery disclosure closes");
            }
            if fixture == "populated" && name == "extensions" {
                assert_extension_filter(
                    &root,
                    "Search installed",
                    "All installed",
                    width,
                    &format!("installed/{width_name}"),
                );
                let cards = widgets_with_class(&root, "hl-card");
                assert_installed_density(&root, &cards, width, width_name);
            }
            if fixture != "error" && name == "processes" {
                let view = find_column_view(&root).expect("Processes renders its DataTable");
                let table = view
                    .ancestor(gtk::ScrolledWindow::static_type())
                    .and_then(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
                    .expect("process DataTable retains its scrolling viewport");
                assert!(
                    (78..=92).contains(&table.height()),
                    "{width_name} sparse process DataTable allocated {}px outside its compact 80px floor and one-row natural height",
                    table.height(),
                );
                assert_eq!(table.max_content_height(), 320);
                assert!(
                    table.propagates_natural_height(),
                    "{width_name} process DataTable must grow with rows only until its 320px ceiling"
                );
                let visible = view
                    .columns()
                    .iter::<gtk::ColumnViewColumn>()
                    .filter_map(Result::ok)
                    .filter(|column| column.is_visible())
                    .filter_map(|column| column.id().map(|id| id.to_string()))
                    .collect::<Vec<_>>();
                if width == 600 {
                    assert_eq!(visible, ["container", "pid", "command", "__responsive_details:0:2,3,4"],);
                    let details = find_menu_button(&root, "View 3 fields")
                        .expect("narrow Processes exposes optional metrics as an explicit action");
                    assert!(details.is_focusable());
                    let disclosure = details
                        .popover()
                        .and_then(|popover| popover.child())
                        .and_then(|child| child.downcast::<gtk::Label>().ok())
                        .expect("narrow process details retain their disclosure content");
                    assert_eq!(
                        disclosure.text(),
                        "User: developer\nCPU: 2.4\nMemory: 64 MiB",
                        "responsive process details name every hidden field and value"
                    );
                } else {
                    assert_eq!(visible, ["container", "pid", "user", "cpu", "memory", "command"]);
                }
            }
            if fixture == "partial-processes" && name == "processes" {
                let summary = "1 container process snapshot unavailable; available containers remain visible.";
                assert_inline_message(&root, summary);
                let message = find_inline_message(&root, summary).expect("partial process warning");
                assert!(message.has_css_class("tone-warning"));
                let disclosure = find_expander(&root, "Technical details");
                assert_eq!(disclosure.accessible_role(), gtk::AccessibleRole::Button);
                assert!(
                    disclosure.is_focusable(),
                    "{width_name} partial diagnostic is keyboard reachable"
                );
                assert!(
                    !disclosure.is_expanded(),
                    "{width_name} partial diagnostic starts collapsed"
                );
                let table = find_column_view(&root).expect("available process rows remain visible");
                let message_bounds = message.compute_bounds(&root).expect("warning belongs to Top root");
                let table_bounds = table.compute_bounds(&root).expect("process table belongs to Top root");
                assert!(
                    message_bounds.y() + message_bounds.height() <= table_bounds.y(),
                    "{width_name} partial warning follows the table instead of preceding it"
                );
                assert!(
                    vertical_end(&root, &message.clone().upcast()) <= 240,
                    "{width_name} partial warning fell below the first 240px"
                );
                assert!(disclosure.grab_focus(), "{width_name} partial diagnostic accepts focus");
                disclosure.emit_by_name::<()>("activate", &[]);
                settle_toolkit();
                assert!(disclosure.is_expanded(), "{width_name} partial diagnostic opens");
                find_mapped_labelled(&root, "job-worker: container process endpoint did not respond");
                capture(
                    &window,
                    &format!("partial-processes-processes-details-{width_name}"),
                    width,
                    800,
                );
                disclosure.emit_by_name::<()>("activate", &[]);
                settle_toolkit();
                assert!(!disclosure.is_expanded(), "{width_name} partial diagnostic closes");
            }
        }
        if fixture == "populated" && name == "extensions" {
            // Re-expanding from the compact selector must not leave the installed
            // collection carrying its narrow allocation or unused vertical space.
            window.set_default_size(1_200, 800);
            window.set_size_request(1_200, 800);
            settle_toolkit();
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, 1_200);
            root.allocate(1_200, 1_600, -1, None);
            assert_contained(&root, &format!("{fixture}/{name}/wide-after-narrow"));
            capture(
                &window,
                &format!("{capture_fixture}-{name}-wide-after-narrow"),
                1_200,
                800,
            );
            let cards = widgets_with_class(&root, "hl-card");
            assert_installed_density(&root, &cards, 1_200, "wide-after-narrow");
        }
        if fixture == "populated" && name == "extensions" && !catalogue_empty {
            find_toggle(&root, "Discover").set_active(true);
            settle_toolkit();
            send_report(&surface, &mut wire, 100, |event| {
                matches!(event, hl_gui::Event::Toggle { .. })
            });
            apply_until(&mut wire, &mut tree, &mut surface, "Find extensions", |request| {
                panic!("unexpected mode switch request: {request:?}")
            });
            let discover_root = surface.widget().clone().upcast::<gtk::Widget>();
            assert!(has_placeholder(&discover_root, "Search extensions"));
            assert!(!has_placeholder(&discover_root, "Search installed"));
            assert!(has_label(&discover_root, "19 of 20 extensions"));
            let review = find_tooltip_button(&discover_root, "Review the 1.0.0 update for Developer Tool 01");
            let review_access = find_tooltip_button(&discover_root, "Review access requested by Developer Tool 02");
            let review_card = ancestor_with_class(review.upcast_ref(), "hl-card")
                .expect("Discover update action belongs to its card");
            let access_card = ancestor_with_class(review_access.upcast_ref(), "hl-card")
                .expect("Discover access action belongs to its card");
            assert!(review.is_sensitive(), "compatible Discover update is actionable");
            for (label, action) in [("update", &review), ("access", &review_access)] {
                assert_eq!(action.accessible_role(), gtk::AccessibleRole::Button);
                assert!(action.is_focusable(), "Discover {label} action is keyboard reachable");
                assert!(action.grab_focus(), "Discover {label} action accepts keyboard focus");
                assert!(
                    action.has_css_class("size-small"),
                    "Discover {label} action uses the compact tier"
                );
            }
            for (width_name, width) in [("wide", 1_200), ("narrow", 600)] {
                window.set_default_size(width, 800);
                window.set_size_request(width, 800);
                settle_toolkit();
                discover_root.allocate(width, 1_600, -1, None);
                window.queue_draw();
                settle_frame();
                assert_contained(&discover_root, &format!("discover/extensions/{width_name}"));
                for (label, action) in [("update", &review), ("access", &review_access)] {
                    assert_eq!(
                        action.height(),
                        28,
                        "{width_name} Discover {label} action control height"
                    );
                }
                if width == 1_200 {
                    let review_card_bounds = review_card
                        .compute_bounds(&discover_root)
                        .expect("Discover update card belongs to Top root");
                    let access_card_bounds = access_card
                        .compute_bounds(&discover_root)
                        .expect("Discover access card belongs to Top root");
                    assert_eq!(
                        review_card_bounds.y(),
                        access_card_bounds.y(),
                        "wide Discover cards must share a grid row"
                    );
                    assert_eq!(
                        review_card_bounds.height(),
                        access_card_bounds.height(),
                        "wide Discover cards must have equal row height"
                    );
                    let review_bounds = review
                        .compute_bounds(&discover_root)
                        .expect("Discover update action belongs to Top root");
                    let access_bounds = review_access
                        .compute_bounds(&discover_root)
                        .expect("Discover access action belongs to Top root");
                    assert!(
                        (review_bounds.y() - access_bounds.y()).abs() <= 16.0,
                        "wide Discover summary actions diverged vertically: update={} access={}",
                        review_bounds.y(),
                        access_bounds.y()
                    );
                    assert!(
                        (review_card.width() - access_card.width()).abs() <= 1,
                        "wide Discover columns must share the available content width"
                    );
                } else {
                    let heights = [review_card.height(), access_card.height()];
                    assert!(
                        heights.iter().all(|height| *height <= 180),
                        "narrow Discover cards stretched sparse content into {heights:?}px panels"
                    );
                    assert_eq!(
                        review_card.width(),
                        access_card.width(),
                        "narrow Discover cards must use one consistent full-width column"
                    );
                }
                let right_edge = widgets_with_class(&discover_root, "hl-card")
                    .into_iter()
                    .filter_map(|card| card.compute_bounds(&discover_root))
                    .map(|bounds| bounds.x() + bounds.width())
                    .max_by(f32::total_cmp)
                    .expect("Discover renders cards");
                assert!(
                    right_edge >= width as f32 - if width == 600 { 17.0 } else { 77.0 },
                    "{width_name} Discover grid stopped at {right_edge}px in a {width}px surface"
                );
                assert!(
                    vertical_end(&discover_root, review.upcast_ref()) <= 800,
                    "{width_name} Discover update action fell below the first viewport"
                );
                capture(&window, &format!("discover-extensions-{width_name}"), width, 800);
                assert_extension_filter(
                    &discover_root,
                    "Search extensions",
                    "Available & updates",
                    width,
                    &format!("discover/{width_name}"),
                );
            }
            exercise_extension_update(&mut wire, &mut tree, &mut surface, &window);
        }
        if fixture == "error" && name == "networks" {
            find_expander(&root, "Technical details").set_expanded(true);
            settle_toolkit();
            assert!(has_label(&root, "workspace daemon socket refused the connection"));
        }
        if fixture == "populated" && name == "networks" {
            let network_id = "c".repeat(32);
            let container_id = "a".repeat(64);
            let mut inspections = 0;
            find_button(&root, "Manage connections").emit_clicked();
            settle_toolkit();
            let interaction = surface
                .reports()
                .drain()
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Invoke { .. }))
                .expect("Manage connections emits an invocation");
            let payload = codec::interaction(&interaction, Some(""))
                .expect("Manage connections invocation has a wire representation");
            wire.send(&Frame::new(ChannelId::new(97), hl_extension::Kind::Event, payload))
                .expect("Manage connections invocation reaches Top");
            let deadline = Instant::now() + DEADLINE;
            while Instant::now() < deadline
                && !has_label(surface.widget().upcast_ref::<gtk::Widget>(), "Refresh connections")
            {
                match receive_until(&mut wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                    Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                    Ok(frame) => {
                        let request = codec::read_request(&frame).expect("expanded network request decodes");
                        let reply = match request {
                            Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                                tree.apply(&frame, &mut surface)
                                    .expect("expanded network frame applies");
                                Reply::Done
                            }
                            Request::NetworkInspect { reference } => {
                                inspections += 1;
                                Reply::Network(NetworkSummary {
                                    id: reference,
                                    name: "development".into(),
                                    driver: "bridge".into(),
                                    scope: "local".into(),
                                    kind: NetworkKind::Custom,
                                    endpoints: Some(NetworkEndpointInventory {
                                        containers: Vec::new(),
                                        truncated: false,
                                    }),
                                })
                            }
                            other => panic!("unexpected expanded network call: {other:?}"),
                        };
                        wire.send(&codec::reply(&reply).expect("expanded reply encodes"))
                            .expect("expanded reply sends");
                    }
                    Err(hl_extension::Transit::Pending) => settle_toolkit(),
                    Err(error) => panic!("expanded network socket failed: {error:?}"),
                }
            }
            let expanded_root = surface.widget().clone().upcast::<gtk::Widget>();
            window.set_child(Some(&expanded_root));
            for (width_name, width) in [("narrow", 600), ("wide", 1_200)] {
                window.set_default_size(width, 800);
                window.set_size_request(width, 800);
                window.present();
                settle_toolkit();
                expanded_root.measure(gtk::Orientation::Horizontal, -1);
                expanded_root.measure(gtk::Orientation::Vertical, width);
                expanded_root.allocate(width, 1_600, -1, None);
                window.queue_draw();
                settle_frame();
                assert_contained(&expanded_root, &format!("expanded/networks/{width_name}"));
                let card = widgets_with_class(&expanded_root, "hl-card")
                    .into_iter()
                    .next()
                    .expect("expanded network remains in its card");
                assert!(
                    card.height() <= 310,
                    "{width_name} empty membership inflated the network card to {}px",
                    card.height()
                );
                find_labelled(&expanded_root, "Immutable network ID");
                let identity = find_label(&expanded_root, &network_id);
                assert!(
                    identity.has_css_class("hl-code"),
                    "{width_name} complete network identity remains selectable code"
                );
                assert!(
                    identity.width() <= card.width() - 20,
                    "{width_name} complete network identity escaped its card: identity={} card={}",
                    identity.width(),
                    card.width()
                );
                find_labelled(&expanded_root, "No containers connected.");
                capture(&window, &format!("expanded-networks-{width_name}"), width, 800);
            }
            assert!(has_label(&expanded_root, "Connected containers · 0"));
            assert!(has_label(&expanded_root, "Refresh connections"));
            let refresh_connections = find_button(&expanded_root, "Refresh connections");
            assert!(refresh_connections.has_css_class("size-small"));
            assert!(refresh_connections.allocation().height() <= 40);
            assert!(!find_expander(&expanded_root, "Danger zone").is_expanded());
            assert_label_order(
                &expanded_root,
                &["Network details", "Container attachment", "Danger zone"],
            );

            let selector = find_toggle(&expanded_root, "Choose…");
            selector.set_active(true);
            settle_toolkit();
            choice_option(&selector, 0).emit_clicked();
            settle_toolkit();
            let selection = send_report(&surface, &mut wire, 97, |event| {
                matches!(event, hl_gui::Event::Change { .. })
            });
            let hl_gui::Event::Change { value, .. } = selection else {
                unreachable!()
            };
            assert_eq!(value, hl_gui::PropValue::Text(container_id.clone()));
            apply_until(&mut wire, &mut tree, &mut surface, "Connect", |request| match request {
                other => panic!("unexpected container selection call: {other:?}"),
            });
            let selected_root = surface.widget().clone().upcast::<gtk::Widget>();
            find_button(&selected_root, "Connect").emit_clicked();
            settle_toolkit();
            send_report(&surface, &mut wire, 99, |event| {
                matches!(event, hl_gui::Event::Invoke { .. })
            });
            let success = "Connected api-worker to development".to_owned();
            apply_until(&mut wire, &mut tree, &mut surface, &success, |request| match request {
                Request::NetworkConnect {
                    reference,
                    container,
                    aliases,
                } => {
                    assert_eq!(reference, network_id);
                    assert_eq!(container, container_id);
                    assert!(aliases.is_empty());
                    Reply::Done
                }
                Request::NetworkList => Reply::Networks(NetworkInventory::bounded(vec![NetworkSummary {
                    id: network_id.clone(),
                    name: "development".into(),
                    driver: "bridge".into(),
                    scope: "local".into(),
                    kind: NetworkKind::Custom,
                    endpoints: Some(NetworkEndpointInventory {
                        containers: vec![container_id.clone()],
                        truncated: false,
                    }),
                }])),
                Request::NetworkInspect { reference } => {
                    inspections += 1;
                    assert_eq!(reference, network_id);
                    Reply::Network(NetworkSummary {
                        id: reference,
                        name: "development".into(),
                        driver: "bridge".into(),
                        scope: "local".into(),
                        kind: NetworkKind::Custom,
                        endpoints: Some(NetworkEndpointInventory {
                            containers: vec![container_id.clone()],
                            truncated: false,
                        }),
                    })
                }
                other => panic!("unexpected post-connect call: {other:?}"),
            });
            apply_until(
                &mut wire,
                &mut tree,
                &mut surface,
                "Connected containers · 1",
                |request| match request {
                    Request::NetworkInspect { reference } => {
                        inspections += 1;
                        assert_eq!(reference, network_id);
                        Reply::Network(NetworkSummary {
                            id: reference,
                            name: "development".into(),
                            driver: "bridge".into(),
                            scope: "local".into(),
                            kind: NetworkKind::Custom,
                            endpoints: Some(NetworkEndpointInventory {
                                containers: vec![container_id.clone()],
                                truncated: false,
                            }),
                        })
                    }
                    other => panic!("unexpected membership verification call: {other:?}"),
                },
            );
            assert!(inspections >= 2, "success was not followed by membership reinspection");
            let success_root = surface.widget().clone().upcast::<gtk::Widget>();
            window.set_child(Some(&success_root));
            for (width_name, width) in [("narrow", 600), ("wide", 1_200)] {
                window.set_default_size(width, 800);
                window.set_size_request(width, 800);
                window.present();
                settle_toolkit();
                success_root.measure(gtk::Orientation::Horizontal, -1);
                success_root.measure(gtk::Orientation::Vertical, width);
                success_root.allocate(width, 1_600, -1, None);
                window.queue_draw();
                settle_frame();
                assert_contained(&success_root, &format!("post-success/networks/{width_name}"));
                if width == 1_200 {
                    assert_labels_painted(
                        &window,
                        &success_root,
                        &[
                            "Workspace",
                            "Settings",
                            "Extensions",
                            "Containers",
                            "Processes",
                            "Executions",
                            "Images",
                            "Volumes",
                            "Networks",
                            "Terminals",
                        ],
                    );
                }
                capture(&window, &format!("post-success-networks-{width_name}"), width, 800);
            }
            assert!(has_label(&success_root, &success));
            assert_inline_message(&success_root, &success);
            assert!(has_label(&success_root, "Connected containers · 1"));
            assert!(has_label(
                &success_root,
                &format!("Container · {}", &container_id[..12])
            ));
            assert!(has_label(&success_root, "Technical details"));
            assert!(!find_expander(&success_root, "Danger zone").is_expanded());
            assert_label_order(
                &success_root,
                &[
                    "Network details",
                    "Container attachment",
                    &success,
                    "Technical details",
                    "Danger zone",
                ],
            );
            assert_focus_order(
                &success_root,
                &[
                    "api-worker · aaaaaaaaaaaa · exited",
                    "Disconnect",
                    "Technical details",
                    "Danger zone",
                ],
            );
            find_expander(&success_root, "Technical details").set_expanded(true);
            settle_toolkit();
            assert!(has_label(&success_root, &format!("Container ID · {container_id}")));
            assert!(has_label(&success_root, &format!("Network ID · {network_id}")));
            assert!(!has_placeholder(&success_root, "Aliases, comma-separated (optional)"));
        }
        if fixture == "populated" && name == "executions" {
            let _ = surface.reports().drain();
            find_button(&root, "Details").emit_clicked();
            settle_toolkit();
            let interaction = surface
                .reports()
                .drain()
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Invoke { .. }))
                .expect("execution Details emits an invocation");
            wire.send(&Frame::new(
                ChannelId::new(104),
                hl_extension::Kind::Event,
                codec::interaction(&interaction, Some("")).expect("Details invocation encodes"),
            ))
            .expect("Details invocation reaches Top");
            let deadline = Instant::now() + DEADLINE;
            while Instant::now() < deadline
                && !has_label(
                    surface.widget().upcast_ref::<gtk::Widget>(),
                    "Command · /bin/sh -lc npm test",
                )
            {
                let frame = match receive_until(&mut wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                    Ok(frame) => frame,
                    Err(hl_extension::Transit::Pending) => continue,
                    Err(error) => panic!("execution detail request failed: {error:?}"),
                };
                if frame.kind == hl_extension::Kind::Credit {
                    continue;
                }
                let reply = match codec::read_request(&frame).expect("execution detail request decodes") {
                    Request::ExecutionInspect { id } => Reply::Execution(ExecutionSummary {
                        id,
                        container_id: "a".repeat(64),
                        running: false,
                        exit_code: 0,
                        pid: 412,
                        command: vec!["/bin/sh".into(), "-lc".into(), "npm test".into()],
                        user: "developer".into(),
                    }),
                    Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                        tree.apply(&frame, &mut surface)
                            .expect("execution detail frame applies");
                        Reply::Done
                    }
                    Request::SourceResize { mutation } | Request::SourceResizeAt { mutation, .. } => {
                        if let SourceMutation::Length { source, version, rows } = mutation {
                            let _ = surface.resize(source, version, rows);
                        }
                        Reply::Done
                    }
                    other => panic!("unexpected execution detail request: {other:?}"),
                };
                wire.send(&codec::reply(&reply).expect("execution detail reply encodes"))
                    .expect("execution detail reply sends");
                settle_toolkit();
            }
            let detail_root = surface.widget().clone().upcast::<gtk::Widget>();
            window.set_child(Some(&detail_root));
            assert!(has_label(&detail_root, "Execution summary"));
            assert!(has_label(&detail_root, "Command · /bin/sh -lc npm test"));
            assert!(has_label(&detail_root, "User · developer"));
            let technical = find_expander(&detail_root, "Technical details");
            assert!(!technical.is_expanded(), "raw property table starts disclosed");
            assert!(technical.grab_focus(), "technical disclosure is keyboard reachable");
            for (width_name, width) in [("narrow", 600), ("wide", 1_200)] {
                window.set_default_size(width, 800);
                window.set_size_request(width, 800);
                window.present();
                settle_toolkit();
                detail_root.measure(gtk::Orientation::Horizontal, -1);
                detail_root.measure(gtk::Orientation::Vertical, width);
                detail_root.allocate(width, 1_600, -1, None);
                window.queue_draw();
                settle_toolkit();
                assert!(
                    technical.allocation().width() <= detail_root.allocation().width(),
                    "technical disclosure stays within the {width_name} detail surface"
                );
                let card = widgets_with_class(&detail_root, "hl-card")
                    .into_iter()
                    .next()
                    .expect("execution detail remains inside its record card");
                let minimum = if width == 1_200 { 900 } else { 540 };
                assert!(
                    card.width() >= minimum,
                    "{width_name} execution detail collapsed to {}px",
                    card.width()
                );
                capture(&window, &format!("execution-detail-{width_name}"), width, 800);
            }
        }
        if fixture == "populated" && name == "images" {
            let _ = surface.reports().drain();
            find_button(&root, "Inspect").emit_clicked();
            settle_toolkit();
            let interaction = surface
                .reports()
                .drain()
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Invoke { .. }))
                .expect("image Inspect emits an invocation");
            wire.send(&Frame::new(
                ChannelId::new(107),
                hl_extension::Kind::Event,
                codec::interaction(&interaction, Some("")).expect("image Inspect invocation encodes"),
            ))
            .expect("image Inspect invocation reaches Top");
            let deadline = Instant::now() + DEADLINE;
            while Instant::now() < deadline && !has_label(surface.widget().upcast_ref::<gtk::Widget>(), "Image summary")
            {
                match receive_until(&mut wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                    Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                    Ok(frame) => {
                        let reply = match codec::read_request(&frame).expect("image detail request decodes") {
                            Request::ImageInspect { reference } => Reply::ImageDetails(ImageDetails {
                                id: format!("sha256:{}", "b".repeat(64)),
                                references: vec![reference],
                                created: "2026-09-11T12:00:00Z".into(),
                                size: 8_192_000,
                                os: "linux".into(),
                                architecture: "amd64".into(),
                                entrypoint: vec!["/bin/sh".into(), "-lc".into()],
                                command: vec!["npm test".into()],
                                working_directory: "/workspace".into(),
                                user: "developer".into(),
                            }),
                            Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                                tree.apply(&frame, &mut surface).expect("image detail frame applies");
                                Reply::Done
                            }
                            Request::SourceResize { mutation } | Request::SourceResizeAt { mutation, .. } => {
                                if let SourceMutation::Length { source, version, rows } = mutation {
                                    let _ = surface.resize(source, version, rows);
                                }
                                Reply::Done
                            }
                            other => panic!("unexpected image detail request: {other:?}"),
                        };
                        wire.send(&codec::reply(&reply).expect("image detail reply encodes"))
                            .expect("image detail reply sends");
                        settle_toolkit();
                    }
                    Err(hl_extension::Transit::Pending) => {}
                    Err(error) => panic!("image detail failed: {error:?}"),
                }
            }
            let image_root = surface.widget().clone().upcast::<gtk::Widget>();
            window.set_child(Some(&image_root));
            assert!(has_label(&image_root, "Platform · linux/amd64"));
            assert!(has_label(&image_root, "7.8 MiB"));
            let technical = find_expander(&image_root, "Technical details");
            assert!(!technical.is_expanded());
            assert!(technical.grab_focus(), "image technical details are keyboard reachable");
            for (width_name, width) in [("wide", 1_200), ("narrow", 600)] {
                window.set_default_size(width, 800);
                window.set_size_request(width, 800);
                window.present();
                settle_toolkit();
                assert_contained(&image_root, &format!("image-detail/{width_name}"));
                capture(&window, &format!("image-detail-{width_name}"), width, 800);
            }
            technical.set_expanded(true);
            settle_toolkit();
            assert!(has_label(
                &image_root,
                &format!("Immutable image ID · sha256:{}", "b".repeat(64))
            ));
        }
        if fixture == "populated" && name == "volumes" {
            let _ = surface.reports().drain();
            find_button(&root, "Inspect").emit_clicked();
            settle_toolkit();
            let interaction = surface
                .reports()
                .drain()
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Invoke { .. }))
                .expect("volume Inspect emits an invocation");
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            window.set_child(None::<&gtk::Widget>);
            wire.send(&Frame::new(
                ChannelId::new(105),
                hl_extension::Kind::Event,
                codec::interaction(&interaction, Some("")).expect("Inspect invocation encodes"),
            ))
            .expect("Inspect invocation reaches Top");
            let deadline = Instant::now() + DEADLINE;
            while Instant::now() < deadline && !has_label(surface.widget().upcast_ref::<gtk::Widget>(), "Review access")
            {
                match receive_until(&mut wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                    Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                    Ok(frame) => {
                        let reply = match codec::read_request(&frame).expect("volume recovery request decodes") {
                            Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                                tree.apply(&frame, &mut surface).expect("volume recovery frame applies");
                                Reply::Done
                            }
                            other => panic!("unexpected volume recovery request: {other:?}"),
                        };
                        wire.send(&codec::reply(&reply).expect("volume recovery reply encodes"))
                            .expect("volume recovery reply sends");
                        settle_toolkit();
                    }
                    Err(hl_extension::Transit::Pending) => {}
                    Err(error) => panic!("volume recovery failed: {error:?}"),
                }
            }
            let recovery_root = surface.widget().clone().upcast::<gtk::Widget>();
            let recovery_window = gtk::Window::new();
            recovery_window.set_child(Some(&recovery_root));
            let open = find_button(&recovery_root, "Review access");
            for (width_name, width) in [("narrow", 600), ("wide", 1_200)] {
                recovery_window.set_default_size(width, 820);
                recovery_window.set_size_request(width, 820);
                recovery_window.present();
                settle_toolkit();
                assert_contained(&recovery_root, &format!("volume-recovery/{width_name}"));
                let entry = find_entry_placeholder(&recovery_root, "Volume name");
                let create = find_button(&recovery_root, "Create");
                let refresh = find_tooltip_button(&recovery_root, "Refresh volumes");
                let controls = [
                    entry.clone().upcast::<gtk::Widget>(),
                    create.clone().upcast(),
                    refresh.clone().upcast(),
                ];
                let tops = controls
                    .iter()
                    .map(|control| control.allocation().y())
                    .collect::<Vec<_>>();
                assert!(
                    tops.iter().max().unwrap() - tops.iter().min().unwrap() <= 4,
                    "{width_name} volume creation controls split across rows: {tops:?}"
                );
                assert!(create.has_css_class("size-small"));
                assert!(create.height() <= 32);
                let entry_bounds = entry
                    .compute_bounds(&recovery_root)
                    .expect("volume name entry belongs to the Top root");
                let maximum_start = if width == 1_200 { 220.0 } else { 32.0 };
                assert!(
                    entry_bounds.x() <= maximum_start,
                    "{width_name} volume creation row drifted away from the leading edge: {}px",
                    entry_bounds.x()
                );
                assert!(entry.allocation().x() < create.allocation().x());
                assert!(create.allocation().x() < refresh.allocation().x());
                assert!(entry.grab_focus());
                assert!(refresh.grab_focus());
                assert!(open.has_css_class("size-small"));
                assert!(
                    open.height() <= 32,
                    "{width_name} access recovery action exceeded 32px: {}",
                    open.height()
                );
                capture(&recovery_window, &format!("volume-recovery-{width_name}"), width, 820);
            }
            assert!(open.grab_focus(), "volume recovery action is keyboard reachable");
            let _ = surface.reports().drain();
            open.emit_clicked();
            settle_toolkit();
            let interaction = surface
                .reports()
                .drain()
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Invoke { .. }))
                .expect("Review access emits an invocation");
            wire.send(&Frame::new(
                ChannelId::new(106),
                hl_extension::Kind::Event,
                codec::interaction(&interaction, Some("")).expect("Review access invocation encodes"),
            ))
            .expect("Review access invocation reaches Top");
            let deadline = Instant::now() + DEADLINE;
            while Instant::now() < deadline
                && !has_label(surface.widget().upcast_ref::<gtk::Widget>(), "Installed extensions")
            {
                match receive_until(&mut wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                    Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                    Ok(frame) => {
                        let reply = match codec::read_request(&frame).expect("Extensions route request decodes") {
                            Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                                tree.apply(&frame, &mut surface)
                                    .expect("Extensions route frame applies");
                                Reply::Done
                            }
                            other => panic!("unexpected Extensions route request: {other:?}"),
                        };
                        wire.send(&codec::reply(&reply).expect("Extensions route reply encodes"))
                            .expect("Extensions route reply sends");
                        settle_toolkit();
                    }
                    Err(hl_extension::Transit::Pending) => {}
                    Err(error) => panic!("Extensions route failed: {error:?}"),
                }
            }
            assert!(has_label(
                surface.widget().upcast_ref::<gtk::Widget>(),
                "Installed extensions"
            ));
        }
        let stderr = child.stop();
        assert!(stderr.is_empty(), "{fixture}/{name} wrote to stderr: {stderr}");
        std::fs::remove_file(socket).expect("Top test socket is removed");
    }

    fn exercise_extension_update(
        wire: &mut Wire<UnixStream>,
        tree: &mut Tree,
        surface: &mut Surface,
        window: &gtk::Window,
    ) {
        let reference = "ghcr.io/example/developer-tool-01:1.0.0";
        let old_digest = format!("sha256:{}", "4".repeat(64));
        let next_digest = format!("sha256:{}", "b".repeat(64));
        let root = surface.widget().clone().upcast::<gtk::Widget>();
        find_tooltip_button(&root, "Review the 1.0.0 update for Developer Tool 01").emit_clicked();
        settle_toolkit();
        send_report(surface, wire, 101, |event| {
            matches!(event, hl_gui::Event::Invoke { .. })
        });

        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Couldn’t inspect extension",
            |request| match request {
                Request::ExtensionAcquisitionStart { reference: actual } => {
                    assert_eq!(actual, reference);
                    Reply::ExtensionAcquisitionJob(ExtensionAcquisitionJob {
                        job: "gtk-update".into(),
                    })
                }
                Request::ExtensionAcquisitionStatus { job } => {
                    Reply::ExtensionAcquisition(ExtensionAcquisitionStatus {
                        job,
                        reference: reference.into(),
                        revision: 1,
                        state: "failed".into(),
                        progress: None,
                        candidate: None,
                        error: Some("registry denied the image request".into()),
                    })
                }
                other => panic!("unexpected failed extension review call: {other:?}"),
            },
            || None,
        );
        let failure_root = surface.widget().clone().upcast::<gtk::Widget>();
        assert!(
            has_label(&failure_root, reference),
            "header retains the copyable image reference"
        );
        assert!(
            !has_label(&failure_root, &format!("Image · {reference}")),
            "failure body does not repeat the header identity"
        );
        let retry = find_button(&failure_root, "Retry inspection");
        let technical = find_expander(&failure_root, "Technical details");
        let ready_deadline = Instant::now() + DEADLINE;
        while !retry.is_sensitive() && Instant::now() < ready_deadline {
            match receive_until(
                &mut *wire,
                (Instant::now() + Duration::from_millis(80)).min(ready_deadline),
            ) {
                Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                Ok(frame) => {
                    let reply = match codec::read_request(&frame).expect("failure recovery request decodes") {
                        Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                            tree.apply(&frame, surface).expect("failure recovery frame applies");
                            Reply::Done
                        }
                        other => panic!("unexpected failure recovery request: {other:?}"),
                    };
                    wire.send(&codec::reply(&reply).expect("failure recovery reply encodes"))
                        .expect("failure recovery reply sends");
                    settle_toolkit();
                }
                Err(hl_extension::Transit::Pending) => {}
                Err(error) => panic!("failure recovery render failed: {error:?}"),
            }
        }
        assert!(
            retry.is_sensitive(),
            "Retry becomes actionable after failed inspection settles"
        );
        for (width_name, width) in [("wide", 1_200), ("narrow", 600)] {
            window.set_default_size(width, 800);
            window.set_size_request(width, 800);
            window.set_child(Some(&failure_root));
            window.present();
            settle_toolkit();
            assert_contained(&failure_root, &format!("extension-acquisition-failure/{width_name}"));
            let retry_bounds = retry
                .compute_bounds(&failure_root)
                .expect("retry belongs to failure card");
            let detail_bounds = technical
                .compute_bounds(&failure_root)
                .expect("details belong to failure card");
            assert!(
                retry_bounds.y() < detail_bounds.y(),
                "{width_name} Retry must precede secondary details"
            );
            capture(
                window,
                &format!("extension-acquisition-failure-{width_name}"),
                width,
                800,
            );
        }
        retry.emit_clicked();
        settle_toolkit();
        send_report(surface, wire, 101, |event| {
            matches!(event, hl_gui::Event::Invoke { .. })
        });

        let acquisition_subscribed = Cell::new(false);
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Downloading image · manifest · 1/2 bytes (50%)",
            |request| match request {
                Request::ExtensionAcquisitionStart { reference: actual } => {
                    assert_eq!(
                        actual, reference,
                        "Discover update retains its exact catalogue reference"
                    );
                    Reply::ExtensionAcquisitionJob(ExtensionAcquisitionJob {
                        job: "gtk-update".into(),
                    })
                }
                Request::ExtensionAcquisitionStatus { job } => {
                    assert_eq!(job, "gtk-update");
                    Reply::ExtensionAcquisition(ExtensionAcquisitionStatus {
                        job,
                        reference: reference.into(),
                        revision: 1,
                        state: "pulling".into(),
                        progress: Some(ExtensionAcquisitionProgress {
                            status: "Downloading image".into(),
                            id: Some("manifest".into()),
                            current: Some(1),
                            total: Some(2),
                        }),
                        candidate: None,
                        error: None,
                    })
                }
                Request::EventSubscribe { topic } => {
                    assert_eq!(topic, hl_extension::Topic::ExtensionAcquisitions);
                    acquisition_subscribed.set(true);
                    Reply::Done
                }
                other => panic!("unexpected extension review call: {other:?}"),
            },
            || None,
        );
        let progress_root = surface.widget().clone().upcast::<gtk::Widget>();
        capture_update_surface(window, &progress_root, "update-progress");
        let cancel = find_button(&progress_root, "Cancel inspection");
        assert!(cancel.has_css_class("size-small"));
        assert_eq!(cancel.height(), 28, "inspection cancellation stays compact");
        let cancel_bounds = cancel
            .compute_bounds(&progress_root)
            .expect("cancel action belongs to the progress surface");
        let toolbar = cancel.parent().expect("cancel action remains in its progress toolbar");
        let toolbar_bounds = toolbar
            .compute_bounds(&progress_root)
            .expect("progress toolbar belongs to the progress surface");
        assert!(
            (cancel_bounds.x() + cancel_bounds.width()
                - toolbar_bounds.x()
                - toolbar_bounds.width())
                .abs()
                <= 1.0,
            "cancel action is stranded beside progress text: cancel={cancel_bounds:?}, toolbar={toolbar_bounds:?}"
        );

        assert!(acquisition_subscribed.get(), "progress wait subscribed before review");
        let payload = codec::payload(&Snapshot::ExtensionAcquisitions(
            hl_extension::ExtensionAcquisitionChange {
                job: "gtk-update".into(),
                revision: 2,
                state: "ready".into(),
                coalesced: 0,
            },
        ))
        .expect("ready acquisition snapshot encodes");
        wire.send(&Frame::new(ChannelId::new(105), hl_extension::Kind::Event, payload))
            .expect("ready acquisition snapshot sends");
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review developer-tool-01",
            |request| match request {
                Request::ExtensionAcquisitionStatus { job } => {
                    Reply::ExtensionAcquisition(ExtensionAcquisitionStatus {
                        job,
                        reference: reference.into(),
                        revision: 2,
                        state: "ready".into(),
                        progress: None,
                        candidate: Some(ExtensionCandidate {
                            name: ExtensionName::new("developer-tool-01").expect("valid extension"),
                            version: "1.0.0".into(),
                            image_digest: next_digest.clone(),
                            requested: Grant::new([
                                Capability::FilesystemRead,
                                Capability::ImageRemove,
                                Capability::Interface,
                                Capability::WorkspaceControl,
                                Capability::WorkspaceEnvironmentRead,
                                Capability::VolumeWrite,
                            ]),
                            required: Grant::new([Capability::Interface]),
                            requested_images: ImageGrant {
                                remove: vec![ImageSelector::Reference {
                                    reference: "registry.example/test-runner:3".into(),
                                }],
                                ..ImageGrant::default()
                            },
                            requested_containers: Default::default(),
                            requested_networks: Default::default(),
                            requested_volumes: VolumeGrant {
                                selectors: Vec::new(),
                                create: true,
                            },
                            requested_filesystem: FilesystemGrant {
                                read: vec![FilesystemSelector::Exact {
                                    exact: RelativePath::new("README.md").expect("valid exact path"),
                                }],
                                ..FilesystemGrant::default()
                            },
                            requested_workspace_environment: WorkspaceEnvironmentGrant {
                                read: vec![WorkspaceEnvironmentSelector::Exact {
                                    workspace: "development".into(),
                                    name: "DATABASE_URL".into(),
                                }],
                                write: Vec::new(),
                            },
                            installed_image_digest: Some(old_digest.clone()),
                        }),
                        error: None,
                    })
                }
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected extension ready call: {other:?}"),
            },
            || None,
        );
        drain_extension_renders(wire, tree, surface);

        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        assert!(has_label(&review_root, "No access selected · 10 requested"));
        assert!(has_label(&review_root, "Publisher · Community"));
        assert!(has_label(
            &review_root,
            "Catalogue source · community/developer-tool-01"
        ));
        assert!(has_label(
            &review_root,
            "Publisher is not verified; confirm the catalogue source and reviewed digest. Image changes from sha256:444444444444…44444444; access has been reset."
        ));
        assert!(!has_label(
            &review_root,
            "Direct OCI image · no catalogue publisher verification. Confirm the source and reviewed image digest before granting access."
        ));
        assert!(has_label(&review_root, "Create, start, stop, and delete workspaces"));
        assert!(has_label(
            &review_root,
            "Image removal can delete named images or every unused workspace image. Workspace lifecycle access can create or delete workspaces and start or stop workloads."
        ));
        assert!(has_label(
            &review_root,
            "Required to keep this extension available after the update: Render this extension interface. Select it below to continue."
        ));
        let update = find_button(&review_root, "Update with selected access");
        assert!(!update.is_sensitive(), "mandatory consent cannot be omitted");
        let interface = find_label(&review_root, "Render this extension interface · Required")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("required capability label names its native switch");
        let _ = surface.reports().drain();
        let _: bool = interface.emit_by_name("state-set", &[&true]);
        interface.set_active(true);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 1/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected extension consent call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let volume_create = find_label(&review_root, "Create new volumes")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("volume-create label names its native switch");
        let _ = surface.reports().drain();
        let _: bool = volume_create.emit_by_name("state-set", &[&true]);
        volume_create.set_active(true);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 3/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected volume-create consent call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let volume_capability = find_label(&review_root, "Create and remove volumes")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("volume-create selects its exact capability");
        assert!(volume_capability.is_active());
        let exact_image = find_label(&review_root, "Remove image · registry.example/test-runner:3")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("exact-image label names its native switch");
        let _ = surface.reports().drain();
        let _: bool = exact_image.emit_by_name("state-set", &[&true]);
        exact_image.set_active(true);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 5/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected exact-image consent call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let image_capability = find_label(&review_root, "Remove images")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("image capability label names its native switch");
        let _ = surface.reports().drain();
        let _: bool = image_capability.emit_by_name("state-set", &[&false]);
        image_capability.set_active(false);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 3/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected exact-image clearing call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let exact_image = find_label(&review_root, "Remove image · registry.example/test-runner:3")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("cleared exact-image label names its native switch");
        assert!(
            !exact_image.is_active(),
            "clearing image removal clears its exact selectors"
        );
        let _ = surface.reports().drain();
        let _: bool = exact_image.emit_by_name("state-set", &[&true]);
        exact_image.set_active(true);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 5/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected exact-image reselection call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let exact = find_label(&review_root, "View contents file · README.md")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("exact-file label names its native switch");
        let _ = surface.reports().drain();
        let _: bool = exact.emit_by_name("state-set", &[&true]);
        exact.set_active(true);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 7/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected exact-file consent call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let file_capability = find_label(&review_root, "Read selected workspace files")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("file capability label names its native switch");
        let _ = surface.reports().drain();
        let _: bool = file_capability.emit_by_name("state-set", &[&false]);
        file_capability.set_active(false);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 5/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected exact-file clearing call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let exact = find_label(&review_root, "View contents file · README.md")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("cleared exact-file label names its native switch");
        assert!(
            !exact.is_active(),
            "clearing file read authority clears its exact roots"
        );
        let _ = surface.reports().drain();
        let _: bool = exact.emit_by_name("state-set", &[&true]);
        exact.set_active(true);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 7/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected exact-file reselection call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        let environment = find_label(&review_root, "Read DATABASE_URL in workspace development")
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("exact environment label names its native switch");
        let _ = surface.reports().drain();
        let _: bool = environment.emit_by_name("state-set", &[&true]);
        environment.set_active(true);
        settle_toolkit();
        send_report(surface, wire, 102, |event| {
            matches!(event, hl_gui::Event::Toggle { .. })
        });
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "Review decision · 9/10 selected",
            |request| match request {
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected environment consent call: {other:?}"),
            },
            || None,
        );
        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        assert!(has_label(&review_root, "Update with selected access"));
        capture_update_surface(window, &review_root, "update-review");
        let update = find_button(&review_root, "Update with selected access");
        assert!(update.is_sensitive(), "required consent enables the update");
        update.emit_clicked();
        settle_toolkit();
        send_report(surface, wire, 103, |event| {
            matches!(event, hl_gui::Event::Invoke { .. })
        });

        let committed = Cell::new(false);
        let inventory_event_sent = Cell::new(false);
        apply_extension_update_until(
            wire,
            tree,
            surface,
            "developer-tool-01 updated and verified.",
            |request| match request {
                Request::ExtensionAcquisitionStatus { job } => {
                    Reply::ExtensionAcquisition(ExtensionAcquisitionStatus {
                        job,
                        reference: reference.into(),
                        revision: 2,
                        state: "ready".into(),
                        progress: None,
                        candidate: Some(ExtensionCandidate {
                            name: ExtensionName::new("developer-tool-01").expect("valid extension"),
                            version: "1.0.0".into(),
                            image_digest: next_digest.clone(),
                            requested: Grant::new([
                                Capability::FilesystemRead,
                                Capability::ImageRemove,
                                Capability::Interface,
                                Capability::WorkspaceControl,
                                Capability::WorkspaceEnvironmentRead,
                                Capability::VolumeWrite,
                            ]),
                            required: Grant::new([Capability::Interface]),
                            requested_images: ImageGrant {
                                remove: vec![ImageSelector::Reference {
                                    reference: "registry.example/test-runner:3".into(),
                                }],
                                ..ImageGrant::default()
                            },
                            requested_containers: Default::default(),
                            requested_networks: Default::default(),
                            requested_volumes: VolumeGrant {
                                selectors: Vec::new(),
                                create: true,
                            },
                            requested_filesystem: FilesystemGrant {
                                read: vec![FilesystemSelector::Exact {
                                    exact: RelativePath::new("README.md").expect("valid exact path"),
                                }],
                                ..FilesystemGrant::default()
                            },
                            requested_workspace_environment: WorkspaceEnvironmentGrant {
                                read: vec![WorkspaceEnvironmentSelector::Exact {
                                    workspace: "development".into(),
                                    name: "DATABASE_URL".into(),
                                }],
                                write: Vec::new(),
                            },
                            installed_image_digest: Some(old_digest.clone()),
                        }),
                        error: None,
                    })
                }
                Request::ExtensionUpdate {
                    job,
                    revision,
                    image_digest,
                    granted,
                    filesystem,
                    workspace_environment,
                    images,
                    volumes,
                    ..
                } => {
                    assert_eq!(job, "gtk-update");
                    assert_eq!(revision, 2);
                    assert_eq!(image_digest, next_digest);
                    assert_eq!(
                        granted,
                        Grant::new([
                            Capability::FilesystemRead,
                            Capability::ImageRemove,
                            Capability::Interface,
                            Capability::WorkspaceEnvironmentRead,
                            Capability::VolumeWrite,
                        ])
                    );
                    assert_eq!(
                        volumes,
                        VolumeGrant {
                            selectors: Vec::new(),
                            create: true
                        }
                    );
                    assert_eq!(
                        images,
                        ImageGrant {
                            remove: vec![ImageSelector::Reference {
                                reference: "registry.example/test-runner:3".into(),
                            }],
                            ..ImageGrant::default()
                        }
                    );
                    assert_eq!(
                        workspace_environment,
                        WorkspaceEnvironmentGrant {
                            read: vec![WorkspaceEnvironmentSelector::Exact {
                                workspace: "development".into(),
                                name: "DATABASE_URL".into(),
                            }],
                            write: Vec::new(),
                        }
                    );
                    assert_eq!(
                        filesystem,
                        FilesystemGrant {
                            read: vec![FilesystemSelector::Exact {
                                exact: RelativePath::new("README.md").expect("valid exact path"),
                            }],
                            ..FilesystemGrant::default()
                        }
                    );
                    committed.set(true);
                    let updated = updated_extension(&next_digest);
                    Reply::Extension(updated)
                }
                Request::ExtensionList => Reply::Extensions(if committed.get() {
                    let mut listing = extensions();
                    if let Some(current) = listing.iter_mut().find(|entry| entry.name == "developer-tool-01") {
                        *current = updated_extension(&next_digest);
                    }
                    listing
                } else {
                    extensions()
                }),
                Request::EventUnsubscribe { .. } => Reply::Done,
                other => panic!("unexpected extension update call: {other:?}"),
            },
            || {
                if committed.get() && !inventory_event_sent.replace(true) {
                    Some(Snapshot::Extensions(vec![updated_extension(&next_digest)]))
                } else {
                    None
                }
            },
        );
        drain_extension_renders(wire, tree, surface);
        let success_root = surface.widget().clone().upcast::<gtk::Widget>();
        assert!(!has_label(&success_root, "Installed · update available"));
        assert!(
            find_tooltip_button(&success_root, "Review access requested by Developer Tool 02").is_sensitive(),
            "success restores catalogue actions"
        );
        capture_update_surface(window, &success_root, "update-success");
    }

    fn updated_extension(digest: &str) -> ExtensionSummary {
        ExtensionSummary {
            name: "developer-tool-01".into(),
            image_digest: digest.into(),
            status: "running".into(),
            version: "1.0.0".into(),
            enabled: true,
            pane_providers: Vec::new(),
            granted: Grant::default(),
            images: Default::default(),
            containers: Default::default(),
            networks: Default::default(),
            volumes: Default::default(),
            filesystem: Default::default(),
            workspace_environment: Default::default(),
        }
    }

    fn apply_extension_update_until(
        wire: &mut Wire<UnixStream>,
        tree: &mut Tree,
        surface: &mut Surface,
        wanted: &str,
        mut answer: impl FnMut(Request) -> Reply,
        mut event: impl FnMut() -> Option<Snapshot>,
    ) {
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline && !has_label(surface.widget().upcast_ref(), wanted) {
            match receive_until(wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                Ok(frame) => {
                    let request = codec::read_request(&frame).expect("extension update request decodes");
                    let reply = match request {
                        Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                            tree.apply(&frame, surface).expect("extension update frame applies");
                            Reply::Done
                        }
                        other => answer(other),
                    };
                    wire.send(&codec::reply(&reply).expect("extension update reply encodes"))
                        .expect("extension update reply sends");
                    if let Some(snapshot) = event() {
                        let payload = codec::payload(&snapshot).expect("extension update snapshot encodes");
                        wire.send(&Frame::new(ChannelId::new(105), hl_extension::Kind::Event, payload))
                            .expect("extension update snapshot sends");
                    }
                }
                Err(hl_extension::Transit::Pending) => settle_toolkit(),
                Err(error) => panic!("extension update socket failed: {error:?}"),
            }
            settle_toolkit();
        }
        assert!(
            has_label(surface.widget().upcast_ref(), wanted),
            "extension update never rendered {wanted:?}"
        );
    }

    fn drain_extension_renders(wire: &mut Wire<UnixStream>, tree: &mut Tree, surface: &mut Surface) {
        let deadline = Instant::now() + DEADLINE;
        let mut quiet = 0;
        while Instant::now() < deadline && quiet < 2 {
            match receive_until(wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                Ok(frame) => {
                    quiet = 0;
                    match codec::read_request(&frame).expect("settled extension request decodes") {
                        Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                            tree.apply(&frame, surface).expect("settled extension frame applies");
                            wire.send(&codec::reply(&Reply::Done).expect("render reply encodes"))
                                .expect("render reply sends");
                        }
                        other => panic!("unexpected request while settling extension success: {other:?}"),
                    }
                }
                Err(hl_extension::Transit::Pending) => quiet += 1,
                Err(error) => panic!("extension success settle failed: {error:?}"),
            }
            settle_toolkit();
        }
    }

    fn capture_update_surface(window: &gtk::Window, root: &gtk::Widget, state: &str) {
        window.set_child(None::<&gtk::Widget>);
        for (width_name, width) in [("narrow", 600), ("wide", 1_200)] {
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 800, -1, None);
            let capture_window = gtk::Window::new();
            capture_window.set_child(Some(root));
            capture_window.set_default_size(width, 800);
            capture_window.present();
            settle_toolkit();
            assert_contained(root, &format!("extensions/{state}/{width_name}"));
            if state == "update-review" {
                let update = find_button(root, "Update with selected access");
                assert!(
                    vertical_end(root, update.upcast_ref()) <= 800,
                    "{width_name} update confirmation fell below the first viewport"
                );
                let warnings = widgets_with_class(root, "hl-inlinemessage")
                    .into_iter()
                    .filter(|message| message.has_css_class("tone-warning"))
                    .count();
                assert_eq!(warnings, 3, "{width_name} review repeats risk chrome");
                let product = find_label(root, "Product access · 5/6");
                let clear = find_button(root, "Clear product access");
                let product_bounds = product
                    .compute_bounds(root)
                    .expect("product access summary belongs to review");
                let clear_bounds = clear
                    .compute_bounds(root)
                    .expect("product access reset belongs to review");
                assert!(
                    (product_bounds.y() - clear_bounds.y()).abs() <= 6.0,
                    "{width_name} product reset detached vertically: summary={product_bounds:?}, clear={clear_bounds:?}"
                );
                assert!(
                    root.width() as f32 - clear_bounds.x() - clear_bounds.width() <= 40.0,
                    "{width_name} product reset floated away from the review edge: {clear_bounds:?}"
                );
                assert!(
                    clear.has_css_class("size-small") && clear.height() <= 38,
                    "{width_name} product reset stays compact: {clear_bounds:?}"
                );
                let environment = find_label(root, "Read selected workspace environment values")
                    .mnemonic_widget()
                    .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
                    .expect("reviewed environment permission names its switch");
                let footer_top = vertical_end(root, update.upcast_ref()) - update.height();
                assert!(
                    vertical_end(root, environment.upcast_ref()) + 8 <= footer_top,
                    "{width_name} second permission choice is obscured by the decision footer"
                );
                assert!(
                    environment.grab_focus(),
                    "{width_name} visible permission accepts focus"
                );
            }
            capture(&capture_window, &format!("extensions-{state}-{width_name}"), width, 800);
            capture_window.set_child(None::<&gtk::Widget>);
            capture_window.close();
        }
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
        let mut names = vec![
            "top".to_owned(),
            "storybook".to_owned(),
            "faulted-agent".to_owned(),
            "disabled-linter".to_owned(),
            "developer-tool-01".to_owned(),
        ];
        names.extend((5..50).map(|index| format!("installed-{index:02}")));
        names
            .into_iter()
            .enumerate()
            .map(|(index, name)| ExtensionSummary {
                name: name.clone(),
                image_digest: format!("sha256:{}", format!("{:x}", index % 16).repeat(64)),
                status: if name == "faulted-agent" {
                    "fault:extension process exited".into()
                } else if name == "disabled-linter" {
                    "standby".into()
                } else {
                    "running".into()
                },
                version: if name == "developer-tool-01" {
                    "0.9.0".into()
                } else {
                    "0.4.0".into()
                },
                enabled: name != "disabled-linter",
                pane_providers: if name == "storybook" {
                    vec![PaneProvider {
                        id: ExtensionName::new("playground").expect("valid provider id"),
                        title: "Component playground".into(),
                        icon: Some("applications-graphics-symbolic".into()),
                    }]
                } else if name == "installed-07" {
                    vec![PaneProvider {
                        id: ExtensionName::new("observability").expect("valid provider id"),
                        title: "Observability dashboard".into(),
                        icon: None,
                    }]
                } else {
                    Vec::new()
                },
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
        let mut entries = vec![ExtensionCatalogueEntry {
            id: "storybook".into(),
            title: "Storybook".into(),
            description: "Inspect the Husklet interface component library.".into(),
            version: "0.4.0".into(),
            reference: "ghcr.io/husklet/storybook:0.4.0".into(),
            publisher: "Husklet".into(),
            source: "Built in".into(),
            publisher_verified: true,
            protocol: PROTOCOL,
            architectures: vec!["amd64".into(), "arm64".into()],
        }];
        entries.extend((1..20).map(|index| ExtensionCatalogueEntry {
            id: format!("developer-tool-{index:02}"),
            title: format!("Developer Tool {index:02}"),
            description: format!("A bounded daily developer workflow for task {index:02}."),
            version: "1.0.0".into(),
            reference: format!("ghcr.io/example/developer-tool-{index:02}:1.0.0"),
            publisher: if index % 2 == 0 { "Acme" } else { "Community" }.into(),
            source: format!("community/developer-tool-{index:02}"),
            publisher_verified: false,
            protocol: if index == 19 { PROTOCOL + 1 } else { PROTOCOL },
            architectures: vec!["amd64".into(), "arm64".into()],
        }));
        ExtensionCatalogue {
            entries,
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

    fn settle_frame() {
        std::thread::sleep(Duration::from_millis(20));
        settle_toolkit();
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

    fn assert_inline_message(root: &gtk::Widget, wanted: &str) {
        let message =
            find_inline_message(root, wanted).unwrap_or_else(|| panic!("no InlineMessage labelled {wanted:?}"));
        assert_eq!(message.accessible_role(), gtk::AccessibleRole::Alert);
        let emblem = message.first_child().expect("inline message has a non-color cue");
        let emblem = emblem.downcast::<gtk::Image>().expect("inline message cue is an image");
        assert!(emblem.icon_name().is_some(), "inline message cue has no symbol");
        assert_eq!(emblem.accessible_role(), gtk::AccessibleRole::Presentation);
        assert!(!emblem.can_focus(), "decorative message cue entered keyboard order");
    }

    fn find_inline_message(root: &gtk::Widget, wanted: &str) -> Option<gtk::Box> {
        if let Some(message) = root.downcast_ref::<gtk::Box>().filter(|candidate| {
            candidate.has_css_class("hl-inlinemessage") && has_label(candidate.upcast_ref(), wanted)
        }) {
            return Some(message.clone());
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(message) = find_inline_message(&current, wanted) {
                return Some(message);
            }
        }
        None
    }

    fn find_column_view(root: &gtk::Widget) -> Option<gtk::ColumnView> {
        if let Ok(view) = root.clone().downcast::<gtk::ColumnView>() {
            return Some(view);
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(view) = find_column_view(&current) {
                return Some(view);
            }
        }
        None
    }

    fn find_menu_button(root: &gtk::Widget, wanted: &str) -> Option<gtk::MenuButton> {
        if let Ok(button) = root.clone().downcast::<gtk::MenuButton>() {
            if button.label().as_deref() == Some(wanted) {
                return Some(button);
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(button) = find_menu_button(&current, wanted) {
                return Some(button);
            }
        }
        None
    }

    fn find_labelled(root: &gtk::Widget, wanted: &str) -> gtk::Widget {
        if root
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text() == wanted)
        {
            return root.clone();
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if has_label(&current, wanted) {
                return find_labelled(&current, wanted);
            }
        }
        panic!("label {wanted:?} was not found")
    }

    fn vertical_end(root: &gtk::Widget, widget: &gtk::Widget) -> i32 {
        let mut current = widget.clone();
        let mut bottom = current.height();
        while current != *root {
            bottom += current.allocation().y();
            current = current.parent().expect("label remains below the captured root");
        }
        bottom
    }

    fn assert_label_order(root: &gtk::Widget, wanted: &[&str]) {
        fn collect(root: &gtk::Widget, labels: &mut Vec<String>) {
            if let Some(label) = root.downcast_ref::<gtk::Label>() {
                labels.push(label.text().to_string());
            }
            let mut child = root.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                collect(&current, labels);
            }
        }

        let mut labels = Vec::new();
        collect(root, &mut labels);
        let mut previous = None;
        for expected in wanted {
            let position = labels
                .iter()
                .position(|label| label == expected)
                .unwrap_or_else(|| panic!("label {expected:?} was not found in {labels:?}"));
            if let Some(before) = previous {
                assert!(
                    before < position,
                    "labels are not in visual traversal order {wanted:?}: {labels:?}"
                );
            }
            previous = Some(position);
        }
    }

    fn assert_focus_order(root: &gtk::Widget, wanted: &[&str]) {
        fn collect(root: &gtk::Widget, wanted: &[&str], labels: &mut Vec<String>) {
            if root.is_focusable() {
                if let Some(label) = wanted.iter().find(|label| has_label(root, label)) {
                    if labels.last().is_none_or(|previous| previous != label) {
                        labels.push((*label).to_owned());
                    }
                }
            }
            let mut child = root.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                collect(&current, wanted, labels);
            }
        }

        let mut labels = Vec::new();
        collect(root, wanted, &mut labels);
        assert_eq!(labels, wanted, "focus traversal does not follow the visual order");
    }

    fn has_placeholder(root: &gtk::Widget, wanted: &str) -> bool {
        if root
            .downcast_ref::<gtk::Entry>()
            .and_then(gtk::Entry::placeholder_text)
            .or_else(|| {
                root.downcast_ref::<gtk::SearchEntry>()
                    .and_then(gtk::SearchEntry::placeholder_text)
            })
            .is_some_and(|placeholder| placeholder == wanted)
        {
            return true;
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if has_placeholder(&current, wanted) {
                return true;
            }
        }
        false
    }

    fn find_label(root: &gtk::Widget, wanted: &str) -> gtk::Label {
        if let Some(label) = root.downcast_ref::<gtk::Label>() {
            if label.text() == wanted {
                return label.clone();
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if has_label(&current, wanted) {
                return find_label(&current, wanted);
            }
        }
        panic!("label {wanted:?} was not found")
    }

    fn find_toggle(root: &gtk::Widget, label: &str) -> gtk::ToggleButton {
        find_toggle_optional(root, label).unwrap_or_else(|| panic!("toggle {label:?} was not found"))
    }

    fn widgets_with_class(root: &gtk::Widget, class: &str) -> Vec<gtk::Widget> {
        let mut found = Vec::new();
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if current.has_css_class(class) {
                found.push(current.clone());
            }
            found.extend(widgets_with_class(&current, class));
        }
        found
    }

    fn assert_overview_recovery(root: &gtk::Widget, width: i32, case: &str) {
        let summary = "Workspace inventory lost its connection. No change was assumed.";
        let message = find_inline_message(root, summary).expect("overview recovery uses InlineMessage");
        assert_eq!(message.accessible_role(), gtk::AccessibleRole::Alert);
        let retry = find_button(root, "Retry inventory");
        assert_eq!(retry.accessible_role(), gtk::AccessibleRole::Button);
        assert!(retry.is_focusable(), "{case} overview retry is keyboard reachable");
        assert!(retry.has_css_class("size-small"));
        assert_eq!(retry.height(), 28, "{case} overview retry control height");
        let disclosure = find_expander(root, "Technical details");
        assert_eq!(disclosure.accessible_role(), gtk::AccessibleRole::Button);
        assert!(
            disclosure.is_focusable(),
            "{case} overview diagnostic disclosure is keyboard reachable"
        );
        assert!(!disclosure.is_expanded(), "{case} overview diagnostics start collapsed");

        let first_card = widgets_with_class(root, "hl-card")
            .into_iter()
            .next()
            .expect("overview recovery retains resource cards");
        let message_bounds = message
            .compute_bounds(root)
            .expect("overview recovery belongs to Top root");
        let retry_bounds = retry.compute_bounds(root).expect("overview retry belongs to Top root");
        let disclosure_bounds = disclosure
            .compute_bounds(root)
            .expect("overview disclosure belongs to Top root");
        let card_bounds = first_card
            .compute_bounds(root)
            .expect("overview card belongs to Top root");
        let content_start = if width == 600 { 16.0 } else { 184.0 };
        assert_eq!(
            message_bounds.x(),
            content_start,
            "{case} recovery starts at the page inset"
        );
        assert_eq!(
            message_bounds.x() + message_bounds.width(),
            root.width() as f32 - 16.0,
            "{case} recovery reaches the trailing page inset"
        );
        assert!(
            message_bounds.y() + message_bounds.height() <= retry_bounds.y(),
            "{case} retry overlaps the recovery summary"
        );
        assert!(
            retry_bounds.y() + retry_bounds.height() <= disclosure_bounds.y(),
            "{case} disclosure overlaps the retry action"
        );
        assert!(
            disclosure_bounds.y() + disclosure_bounds.height() <= card_bounds.y(),
            "{case} resource cards precede the recovery controls"
        );
        let maximum_card_y = if width == 600 { 280.0 } else { 240.0 };
        assert!(
            card_bounds.y() <= maximum_card_y,
            "{case} recovery pushed the first resource card to {}px",
            card_bounds.y()
        );
    }

    fn assert_overview_grid(root: &gtk::Widget, width: i32, content_start: i32, case: &str) {
        let cards = widgets_with_class(root, "hl-card");
        assert_eq!(cards.len(), 8, "{case} overview renders every summary card");
        for (card, label) in cards.iter().zip([
            "Containers",
            "Processes",
            "Executions",
            "Images",
            "Volumes",
            "Networks",
            "Terminal tabs",
            "Extensions",
        ]) {
            assert!(card.has_css_class("variant-outline"));
            assert!(card.hexpands(), "{case} {label} card participates in responsive fill");
            assert!(!card.vexpands(), "{case} {label} card keeps content height");
            assert!(
                (96..=104).contains(&card.height()),
                "{case} {label} card was {}px instead of a compact three-line surface",
                card.height()
            );
            let action = find_button(card, label);
            assert_eq!(action.accessible_role(), gtk::AccessibleRole::Button);
            assert!(action.is_focusable(), "{case} {label} summary is keyboard actionable");
        }
        let first_action = find_button(&cards[0], "Containers");
        assert!(first_action.grab_focus(), "{case} first summary accepts native focus");
        assert!(first_action.has_focus(), "{case} first summary exposes its focus state");

        let mut bounds = cards
            .iter()
            .map(|card| {
                let bounds = card.compute_bounds(root).expect("overview card belongs to Top root");
                (
                    bounds.x().round() as i32,
                    bounds.y().round() as i32,
                    bounds.width().round() as i32,
                )
            })
            .collect::<Vec<_>>();
        bounds.sort_unstable_by_key(|(x, y, _)| (*y, *x));
        let mut rows = bounds.iter().map(|(_, y, _)| *y).collect::<Vec<_>>();
        rows.dedup();
        let expected_rows = if width == 600 { 4 } else { 2 };
        let expected_columns = if width == 600 { 2 } else { 4 };
        assert_eq!(rows.len(), expected_rows, "{case} overview row count");
        for y in rows {
            let row = bounds.iter().filter(|(_, top, _)| *top == y).collect::<Vec<_>>();
            assert_eq!(row.len(), expected_columns, "{case} overview column count at y={y}");
            let (first_x, _, _) = *row[0];
            let (last_x, _, last_width) = *row[row.len() - 1];
            assert_eq!(
                first_x, content_start,
                "{case} overview row starts at the content inset"
            );
            assert_eq!(
                last_x + last_width,
                root.width() - 16,
                "{case} overview row reaches the trailing page inset"
            );
            let widths = row.iter().map(|(_, _, width)| *width).collect::<Vec<_>>();
            assert!(
                widths.iter().all(|width| *width >= 200),
                "{case} overview columns fell below a readable 200px: {widths:?}"
            );
            if width == 600 {
                let (second_x, _, _) = *row[1];
                let first_width = row[0].2;
                assert_eq!(second_x - first_x - first_width, 4, "{case} narrow card gap");
                assert!(
                    widths.iter().all(|card_width| (240..=324).contains(card_width)),
                    "{case} narrow overview cards did not share a readable half row: {widths:?}"
                );
            }
        }
        let last = cards.last().expect("overview has a final summary card");
        assert!(
            vertical_end(root, last) <= 600,
            "{case} overview destinations exceeded the compact first-screen budget"
        );
    }

    fn assert_installed_density(root: &gtk::Widget, cards: &[gtk::Widget], width: i32, case: &str) {
        let search = find_search_with_placeholder(root, "Search installed");
        assert!(!cards.is_empty(), "Installed renders cards");
        let first = ancestor_with_class(&find_mapped_labelled(root, "faulted-agent"), "hl-card")
            .expect("faulted installed extension belongs to a card");
        let first_y = vertical_end(root, &first) - first.height();
        let gap = first_y - vertical_end(root, search.upcast_ref());
        let maximum_gap = if width == 600 { 128 } else { 160 };
        assert!(
            gap <= maximum_gap,
            "{case} left {gap}px between the installed filters and first card instead of at most {maximum_gap}px"
        );
        let maximum_y = if width == 600 { 500 } else { 360 };
        assert!(
            first_y <= maximum_y,
            "{case} first installed card began at {}px instead of at most {maximum_y}px",
            first_y
        );
        assert!(
            vertical_end(root, &first) <= 780,
            "{case} first installed card was not completely visible in the 800px viewport"
        );
        for label in ["Retry", "Review update", "Enable"] {
            let Some(action) = find_button_optional(root, label) else {
                assert_eq!(label, "Review update", "{case} omitted required {label} action");
                continue;
            };
            assert_eq!(action.accessible_role(), gtk::AccessibleRole::Button);
            assert!(action.is_focusable(), "{case} {label} action is keyboard reachable");
            assert!(
                action.has_css_class("size-small"),
                "{case} {label} action uses the compact card tier"
            );
            assert_eq!(action.height(), 28, "{case} {label} action control height");
        }
        let expected_visible = if width == 600 { 1 } else { 3 };
        let visible = cards
            .iter()
            .filter(|card| card.is_mapped())
            .take(expected_visible)
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), expected_visible, "{case} omitted first-row cards");
        if width > 600 {
            let widths = cards
                .iter()
                .filter(|card| card.is_mapped())
                .map(gtk::Widget::width)
                .collect::<Vec<_>>();
            let narrowest = widths.iter().min().copied().unwrap_or_default();
            let widest = widths.iter().max().copied().unwrap_or_default();
            assert!(
                widest - narrowest <= 1,
                "{case} installed card grid inherited uneven content widths: {widths:?}"
            );
        }
        assert!(
            visible.iter().all(|card| vertical_end(root, card) <= 780),
            "{case} did not show the complete first installed row in the 800px viewport"
        );
    }

    fn find_search_with_placeholder(root: &gtk::Widget, wanted: &str) -> gtk::SearchEntry {
        fn find(root: &gtk::Widget, wanted: &str) -> Option<gtk::SearchEntry> {
            if let Some(entry) = root
                .downcast_ref::<gtk::SearchEntry>()
                .filter(|entry| entry.is_mapped() && entry.placeholder_text().as_deref() == Some(wanted))
            {
                return Some(entry.clone());
            }
            let mut child = root.first_child();
            while let Some(widget) = child {
                if widget.is_mapped() {
                    if let Some(entry) = find(&widget, wanted) {
                        return Some(entry);
                    }
                }
                child = widget.next_sibling();
            }
            None
        }
        find(root, wanted).unwrap_or_else(|| panic!("entry with placeholder {wanted:?} was not rendered"))
    }

    fn assert_extension_filter(root: &gtk::Widget, placeholder: &str, selected: &str, width: i32, case: &str) {
        fn find_choice(root: &gtk::Widget, selected: &str) -> Option<gtk::Widget> {
            if root.is_mapped() && root.accessible_role() == gtk::AccessibleRole::ComboBox && has_label(root, selected)
            {
                return Some(root.clone());
            }
            let mut child = root.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if let Some(choice) = find_choice(&widget, selected) {
                    return Some(choice);
                }
            }
            None
        }

        fn find_selected_label(root: &gtk::Widget, selected: &str) -> Option<gtk::Label> {
            if let Some(label) = root
                .downcast_ref::<gtk::Label>()
                .filter(|label| label.is_mapped() && label.text() == selected)
            {
                return Some(label.clone());
            }
            let mut child = root.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if let Some(label) = find_selected_label(&widget, selected) {
                    return Some(label);
                }
            }
            None
        }

        fn mapped_widgets(root: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
            if root.is_mapped() {
                found.push(root.clone());
            }
            let mut child = root.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                mapped_widgets(&widget, found);
            }
        }

        let query = find_search_with_placeholder(root, placeholder).upcast::<gtk::Widget>();
        let choice = find_choice(root, selected)
            .unwrap_or_else(|| panic!("{case} selected filter {selected:?} was not rendered"));
        let selected_label = find_selected_label(&choice, selected)
            .unwrap_or_else(|| panic!("{case} selected label {selected:?} was not rendered"));
        assert!(
            !selected_label.layout().is_ellipsized(),
            "{case} selected filter label {selected:?} was ellipsized at label={}px choice={}px natural={:?}",
            selected_label.width(),
            choice.width(),
            selected_label.layout().pixel_size(),
        );

        let bounds = choice
            .compute_bounds(root)
            .unwrap_or_else(|| panic!("{case} filter does not belong to the rendered root"));
        assert!(
            bounds.x() >= 16.0 && bounds.x() + bounds.width() <= (width - 16) as f32,
            "{case} filter escaped the 16px page inset: x={} width={} page={width}",
            bounds.x(),
            bounds.width(),
        );

        let mut ordered = Vec::new();
        mapped_widgets(root, &mut ordered);
        let query_position = ordered
            .iter()
            .position(|widget| widget == &query)
            .unwrap_or_else(|| panic!("{case} query is absent from keyboard traversal"));
        let choice_position = ordered
            .iter()
            .position(|widget| widget == &choice)
            .unwrap_or_else(|| panic!("{case} filter is absent from keyboard traversal"));
        let result_position = ordered
            .iter()
            .position(|widget| widget.is_focusable() && ancestor_with_class(widget, "hl-card").is_some());
        let result_position =
            result_position.unwrap_or_else(|| panic!("{case} has no keyboard-reachable result action"));
        assert!(
            query_position < choice_position && choice_position < result_position,
            "{case} keyboard order was query={query_position}, filter={choice_position}, result={result_position}"
        );
    }

    fn ancestor_with_class(widget: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
        let mut ancestor = Some(widget.clone());
        while let Some(widget) = ancestor {
            if widget.has_css_class(class) {
                return Some(widget);
            }
            ancestor = widget.parent();
        }
        None
    }

    fn find_mapped_labelled(root: &gtk::Widget, wanted: &str) -> gtk::Widget {
        if root.is_mapped()
            && root
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text() == wanted)
        {
            return root.clone();
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            if widget.is_mapped() && has_label(&widget, wanted) {
                return find_mapped_labelled(&widget, wanted);
            }
            child = widget.next_sibling();
        }
        panic!("mapped label {wanted:?} was not rendered")
    }

    fn find_expander(root: &gtk::Widget, label: &str) -> gtk::Expander {
        if let Some(expander) = root.downcast_ref::<gtk::Expander>() {
            if has_label(root, label) {
                return expander.clone();
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(expander) = find_expander_optional(&current, label) {
                return expander;
            }
        }
        panic!("expander {label:?} was not found")
    }

    fn find_expander_optional(root: &gtk::Widget, label: &str) -> Option<gtk::Expander> {
        if let Some(expander) = root.downcast_ref::<gtk::Expander>() {
            if has_label(root, label) {
                return Some(expander.clone());
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(expander) = find_expander_optional(&current, label) {
                return Some(expander);
            }
        }
        None
    }

    fn find_toggle_optional(root: &gtk::Widget, label: &str) -> Option<gtk::ToggleButton> {
        if let Some(toggle) = root.downcast_ref::<gtk::ToggleButton>() {
            if has_label(root, label) {
                return Some(toggle.clone());
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(toggle) = find_toggle_optional(&current, label) {
                return Some(toggle);
            }
        }
        None
    }

    fn choice_option(choice: &gtk::ToggleButton, index: u32) -> gtk::Button {
        let overlay = choice
            .child()
            .and_downcast::<gtk::Overlay>()
            .expect("choice has an overlay");
        let mut child = overlay.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Ok(popover) = current.downcast::<gtk::Popover>() {
                let options = popover
                    .child()
                    .and_downcast::<gtk::Box>()
                    .expect("choice options are boxed");
                return (0..index)
                    .try_fold(options.first_child().expect("choice has an option"), |option, _| {
                        option.next_sibling()
                    })
                    .expect("choice option exists")
                    .downcast::<gtk::Button>()
                    .expect("choice option is a button");
            }
        }
        panic!("choice popover was not found")
    }

    fn send_report(
        surface: &Surface,
        wire: &mut Wire<UnixStream>,
        channel: u32,
        wanted: impl Fn(&hl_gui::Event) -> bool,
    ) -> hl_gui::Event {
        let event = surface
            .reports()
            .drain()
            .into_iter()
            .find(wanted)
            .expect("live GTK control emits the expected report");
        let payload =
            codec::interaction(&event, Some("")).expect("live GTK report has a production wire representation");
        wire.send(&Frame::new(ChannelId::new(channel), hl_extension::Kind::Event, payload))
            .expect("live GTK report reaches Top");
        event
    }

    fn apply_until(
        wire: &mut Wire<UnixStream>,
        tree: &mut Tree,
        surface: &mut Surface,
        wanted: &str,
        mut answer: impl FnMut(Request) -> Reply,
    ) {
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline && !has_label(surface.widget().upcast_ref::<gtk::Widget>(), wanted) {
            match receive_until(wire, (Instant::now() + Duration::from_millis(80)).min(deadline)) {
                Ok(frame) if frame.kind == hl_extension::Kind::Credit => {}
                Ok(frame) => {
                    let request = codec::read_request(&frame).expect("interactive request decodes");
                    let reply = match request {
                        Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                            tree.apply(&frame, surface).expect("interactive frame applies");
                            Reply::Done
                        }
                        other => answer(other),
                    };
                    wire.send(&codec::reply(&reply).expect("interactive reply encodes"))
                        .expect("interactive reply sends");
                }
                Err(hl_extension::Transit::Pending) => settle_toolkit(),
                Err(error) => panic!("interactive socket failed: {error:?}"),
            }
        }
        assert!(has_label(surface.widget().upcast_ref::<gtk::Widget>(), wanted));
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

    fn find_combobox(root: &gtk::Widget) -> gtk::Widget {
        if root.is_mapped() && root.accessible_role() == gtk::AccessibleRole::ComboBox {
            return root.clone();
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(found) = find_combobox_optional(&current) {
                return found;
            }
        }
        panic!("compact section chooser was not rendered")
    }

    fn find_combobox_optional(root: &gtk::Widget) -> Option<gtk::Widget> {
        if root.is_mapped() && root.accessible_role() == gtk::AccessibleRole::ComboBox {
            return Some(root.clone());
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(found) = find_combobox_optional(&current) {
                return Some(found);
            }
        }
        None
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

    fn assert_labels_painted(window: &gtk::Window, root: &gtk::Widget, labels: &[&str]) {
        let paintable = gtk::WidgetPaintable::new(Some(window.upcast_ref::<gtk::Widget>()));
        let node = (0..20)
            .find_map(|_| {
                window.queue_draw();
                settle_toolkit();
                let snapshot = gtk::Snapshot::new();
                paintable.snapshot(snapshot.upcast_ref::<gtk::gdk::Snapshot>(), 1_200.0, 800.0);
                let node = snapshot.to_node();
                if node.is_none() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                node
            })
            .expect("Top window produces a render node");
        let renderer = window.renderer().expect("Top window has a renderer");
        let texture = renderer.render_texture(&node, None);
        let width = texture.width() as usize;
        let height = texture.height() as usize;
        let stride = width * 4;
        let mut pixels = vec![0_u8; stride * height];
        texture.download(&mut pixels, stride);
        let mut previous_bottom = 0.0_f32;
        for wanted in labels {
            let destination = widgets_with_class(root, "hl-navigationmenuitem")
                .into_iter()
                .find(|widget| has_label(widget, wanted))
                .unwrap_or_else(|| panic!("navigation destination {wanted:?} is rendered"));
            let label = find_mapped_labelled(&destination, wanted);
            let bounds = label
                .compute_bounds(root)
                .expect("navigation label belongs to Top root");
            assert!(
                bounds.height() >= 10.0,
                "navigation label {wanted:?} was mapped without a drawable line: {bounds:?}"
            );
            assert!(
                bounds.y() >= previous_bottom,
                "navigation label {wanted:?} overlaps the destination before it: {bounds:?}"
            );
            previous_bottom = bounds.y() + bounds.height();
            let x0 = bounds.x().floor().max(0.0) as usize;
            let y0 = bounds.y().floor().max(0.0) as usize;
            let x1 = (bounds.x() + bounds.width()).ceil().min(width as f32) as usize;
            let y1 = (bounds.y() + bounds.height()).ceil().min(height as f32) as usize;
            let painted = (y0..y1).any(|y| {
                (x0..x1).any(|x| {
                    let pixel = &pixels[y * stride + x * 4..y * stride + x * 4 + 3];
                    pixel.iter().all(|channel| *channel >= 128)
                })
            });
            assert!(
                painted,
                "navigation label {wanted:?} was mapped but absent from the rendered frame"
            );
        }
    }

    fn find_button(root: &gtk::Widget, label: &str) -> gtk::Button {
        if let Some(button) = root.downcast_ref::<gtk::Button>() {
            if has_label(root, label) {
                return button.clone();
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(button) = find_button_optional(&current, label) {
                return button;
            }
        }
        panic!("button {label:?} was not rendered");
    }

    fn find_entry_placeholder(root: &gtk::Widget, placeholder: &str) -> gtk::Entry {
        fn find(root: &gtk::Widget, placeholder: &str) -> Option<gtk::Entry> {
            if let Some(entry) = root
                .downcast_ref::<gtk::Entry>()
                .filter(|entry| entry.placeholder_text().as_deref() == Some(placeholder))
            {
                return Some(entry.clone());
            }
            let mut child = root.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                if let Some(entry) = find(&current, placeholder) {
                    return Some(entry);
                }
            }
            None
        }
        find(root, placeholder).unwrap_or_else(|| panic!("entry placeholder {placeholder:?} was not rendered"))
    }

    fn find_heading(root: &gtk::Widget, wanted: &str) -> gtk::Label {
        fn find(root: &gtk::Widget, wanted: &str) -> Option<gtk::Label> {
            if let Some(label) = root
                .downcast_ref::<gtk::Label>()
                .filter(|label| label.text() == wanted && label.has_css_class("hl-heading"))
            {
                return Some(label.clone());
            }
            let mut child = root.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                if let Some(label) = find(&current, wanted) {
                    return Some(label);
                }
            }
            None
        }
        find(root, wanted).unwrap_or_else(|| panic!("heading {wanted:?} was not rendered"))
    }

    fn find_tooltip_button(root: &gtk::Widget, tooltip: &str) -> gtk::Button {
        let mut pending = vec![root.clone()];
        while let Some(widget) = pending.pop() {
            if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
                if button.tooltip_text().as_deref() == Some(tooltip) {
                    return button;
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                pending.push(current);
            }
        }
        panic!("no button with tooltip {tooltip:?}");
    }

    fn find_button_optional(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = root.downcast_ref::<gtk::Button>() {
            if has_label(root, label) {
                return Some(button.clone());
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(button) = find_button_optional(&current, label) {
                return Some(button);
            }
        }
        None
    }

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crate lives below repository/src/workspaces")
            .to_path_buf()
    }
}
