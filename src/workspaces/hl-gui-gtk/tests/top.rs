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
        ExtensionAcquisitionJob, ExtensionAcquisitionProgress, ExtensionAcquisitionStatus, ExtensionCandidate,
        ExtensionCatalogue, ExtensionCatalogueEntry, NetworkEndpointInventory, NetworkInventory, NetworkKind,
        NetworkSummary,
    };
    use hl_extension::{
        codec, Capability, ChannelId, ExtensionName, ExtensionPreferences, ExtensionSummary, Frame, Grant, Hello,
        PaneProvider, PreferenceValue, Reply, Request, Snapshot, Welcome, Wire, WorkspaceConfiguration, WorkspaceInfo,
        WorkspaceTerminal, PROTOCOL,
    };
    use hl_gui::{Renderer as _, SourceMutation, Theme, Tree};
    use hl_gui_gtk::Surface;

    const CASES: &[(&str, &str)] = &[
        ("workspace", "workspace"),
        ("settings", "settings"),
        ("extensions", "extensions"),
        ("processes", "processes"),
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
                    Capability::VolumeRead,
                    Capability::NetworkRead,
                    Capability::NetworkWrite,
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
            window.set_default_size(width, 800);
            window.set_size_request(width, 800);
            window.present();
            settle_toolkit();
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 1_600, -1, None);
            assert_eq!(root.width(), width, "{fixture}/{name} rejected {width}px");
            assert_contained(&root, &format!("{fixture}/{name}/{width_name}"));
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
                let refresh = find_button(&root, "Refresh");
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
            }
            if fixture == "populated" && name == "processes" && width == 1_200 {
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
                    let request = codec::read_request(&frame).expect("concurrent Top call decodes");
                    let mut delivered = false;
                    let reply = match request {
                        Request::InterfaceRender { frame } | Request::InterfaceRenderAt { frame, .. } => {
                            tree.apply(&frame, &mut surface).expect("process rerender applies");
                            Reply::Done
                        }
                        Request::SourceResize { mutation } | Request::SourceResizeAt { mutation, .. } => {
                            if let SourceMutation::Window(window) = mutation {
                                surface.rows(&window).expect("GTK accepts process rows");
                                delivered = true;
                            }
                            Reply::Done
                        }
                        other => panic!("unexpected call awaiting process rows: {other:?}"),
                    };
                    wire.send(&codec::reply(&reply).expect("concurrent reply encodes"))
                        .expect("concurrent reply sends");
                    if delivered {
                        break;
                    }
                }
                settle_toolkit();
            }
            if fixture == "populated" && name == "workspace" && width == 1_200 {
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
            }
            capture(&window, &format!("{capture_fixture}-{name}-{width_name}"), width, 800);
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
            if fixture == "populated" && name == "processes" {
                let view = find_column_view(&root).expect("Processes renders its DataTable");
                let table = view
                    .ancestor(gtk::ScrolledWindow::static_type())
                    .and_then(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
                    .expect("process DataTable retains its scrolling viewport");
                assert!(
                    (318..=322).contains(&table.height()),
                    "{width_name} process DataTable allocated {}px instead of authored 320px",
                    table.height(),
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
                    let details =
                        find_menu_button(&root, "3 details").expect("narrow Processes exposes optional metrics");
                    assert!(details.is_focusable());
                } else {
                    assert_eq!(visible, ["container", "pid", "user", "cpu", "memory", "command"]);
                }
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
            assert!(review.is_sensitive(), "compatible Discover update is actionable");
            for (width_name, width) in [("wide", 1_200), ("narrow", 600)] {
                window.set_default_size(width, 800);
                window.set_size_request(width, 800);
                settle_toolkit();
                discover_root.allocate(width, 1_600, -1, None);
                assert_contained(&discover_root, &format!("discover/extensions/{width_name}"));
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
                assert_contained(&expanded_root, &format!("expanded/networks/{width_name}"));
                capture(&window, &format!("expanded-networks-{width_name}"), width, 800);
            }
            assert!(has_label(&expanded_root, "Connected containers · 0"));
            assert!(has_label(&expanded_root, "Refresh connections"));
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
                assert_contained(&success_root, &format!("post-success/networks/{width_name}"));
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
                            requested: Grant::new([Capability::ContainerRead]),
                            requested_images: Default::default(),
                            requested_containers: Default::default(),
                            requested_networks: Default::default(),
                            requested_volumes: Default::default(),
                            requested_filesystem: Default::default(),
                            requested_workspace_environment: Default::default(),
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

        let review_root = surface.widget().clone().upcast::<gtk::Widget>();
        assert!(has_label(&review_root, "No access selected · 1 requested"));
        assert!(has_label(&review_root, "Update with selected access"));
        capture_update_surface(window, &review_root, "update-review");
        find_button(&review_root, "Update with selected access").emit_clicked();
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
                            requested: Grant::new([Capability::ContainerRead]),
                            requested_images: Default::default(),
                            requested_containers: Default::default(),
                            requested_networks: Default::default(),
                            requested_volumes: Default::default(),
                            requested_filesystem: Default::default(),
                            requested_workspace_environment: Default::default(),
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
                    ..
                } => {
                    assert_eq!(job, "gtk-update");
                    assert_eq!(revision, 2);
                    assert_eq!(image_digest, next_digest);
                    assert!(granted.is_empty(), "review begins with no implicit access");
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
                assert!(
                    vertical_end(root, &find_labelled(root, "Update with selected access")) <= 800,
                    "{width_name} update confirmation fell below the first viewport"
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
        let expected_visible = if width == 600 { 1 } else { 3 };
        let visible = cards
            .iter()
            .filter(|card| card.is_mapped())
            .take(expected_visible)
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), expected_visible, "{case} omitted first-row cards");
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
