//! Real Storybook process → socket protocol → retained tree → GTK adapter.

#[cfg(unix)]
mod unix {
    use std::io::{self, Read as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use hl_extension::{
        Capability, ChannelId, ExtensionName, Frame, Grant, Hello, Kind, PROTOCOL, Reply, Request, Welcome, Wire, codec,
    };
    use hl_gui::{LOG_VIEW_CHARACTER_LIMIT, Renderer as _, SourceMutation, Theme, Tree};
    use hl_gui_gtk::Surface;

    const STORIES: &[&str] = &[
        "Autocomplete",
        "Button",
        "Card",
        "IconButton",
        "Entry",
        "Search",
        "NumberEntry",
        "PasswordEntry",
        "TextArea",
        "Select",
        "Switch",
        "ToggleButton",
        "Checkbox",
        "Radio",
        "RadioGroup",
        "FormControl",
        "Slider",
        "Heading",
        "Expander",
        "InlineMessage",
        "RecoveryState",
        "Extension acquisition",
        "Validated settings form",
        "Keyboard and semantic actions",
        "Drag and keyboard reorder",
        "Workspace layout control",
        "DataTable",
        "Navigation and transient UI",
        "Bounded streaming log",
        "Virtual event timeline",
        "Bounded key/value inspector",
    ];
    const PATCH_LIMIT: usize = 1_200;
    const PRIMARY_SLOT: &str = "";
    const SOCKET_DEADLINE: Duration = Duration::from_secs(5);
    const SOCKET_TICK: Duration = Duration::from_millis(100);

    struct StorybookChild(Child);

    impl StorybookChild {
        fn stop(&mut self) -> (std::process::ExitStatus, String) {
            if self.0.try_wait().expect("Storybook process status reads").is_none() {
                self.0.kill().expect("Storybook test process stops");
            }
            let status = self.0.wait().expect("Storybook process is reaped");
            let mut stderr = String::new();
            self.0
                .stderr
                .take()
                .expect("captured stderr")
                .read_to_string(&mut stderr)
                .expect("stderr reads after the process exits");
            (status, stderr)
        }
    }

    impl Drop for StorybookChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    #[test]
    fn every_composed_story_crosses_the_real_socket_and_renders_narrow_and_wide() {
        assert!(gtk::init().is_ok(), "run this test under Xvfb");
        let repository = repository();
        assert!(
            repository.join("extensions/node_modules/@husklet/react").exists(),
            "run `npm --prefix extensions ci` before the GTK Storybook E2E"
        );
        for (index, story) in STORIES.iter().enumerate() {
            render_story(&repository, story, index);
        }
    }

    fn render_story(repository: &Path, story: &str, index: usize) {
        let socket = std::env::temp_dir().join(format!("husklet-storybook-{}-{index}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("the Storybook test socket binds");
        listener
            .set_nonblocking(true)
            .expect("the Storybook listener becomes deadline-bound");
        let mut child = StorybookChild(
            Command::new("node")
                .arg(repository.join("extensions/storybook/dist/main.js"))
                .env("HUSKLET_EXTENSION_SOCKET", &socket)
                .env("HUSKLET_STORYBOOK_STORY", story)
                .current_dir(repository.join("extensions/storybook"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the real Storybook entrypoint starts"),
        );
        let stream = accept_before(&listener, Instant::now() + SOCKET_DEADLINE).unwrap_or_else(|error| {
            panic!(
                "Storybook connects to the host socket: {error}; stderr: {}",
                child.stop().1
            )
        });
        stream
            .set_read_timeout(Some(SOCKET_TICK))
            .expect("Storybook reads become deadline-bound");
        stream
            .set_write_timeout(Some(SOCKET_DEADLINE))
            .expect("Storybook writes become deadline-bound");
        let mut wire = Wire::new(stream);
        wire.send(
            &codec::welcome(&Welcome {
                protocol: PROTOCOL,
                host: "storybook-e2e".into(),
                workspace: "test".into(),
                peer: ExtensionName::new("storybook").expect("valid extension name"),
                granted: Grant::new([Capability::Interface]),
                filesystem: hl_extension::FilesystemGrant::default(),
                containers: hl_extension::ContainerGrant::default(), images: hl_extension::ImageGrant::default(),
                networks: hl_extension::NetworkGrant::default(), volumes: hl_extension::VolumeGrant::default(),
                limits: hl_extension::Limits::default(),
            })
            .expect("welcome encodes"),
        )
        .expect("welcome is sent");
        let hello: Hello =
            codec::read_hello(&receive_before(&mut wire, SOCKET_DEADLINE).expect("Storybook greets the host"))
                .expect("hello decodes");
        assert_eq!(hello.protocol, PROTOCOL);

        let mut rendered = Vec::new();
        let mut ready = false;
        let startup_deadline = Instant::now() + SOCKET_DEADLINE;
        for _ in 0..8 {
            let carried = receive_until(&mut wire, startup_deadline)
                .unwrap_or_else(|error| panic!("{story} sends a bounded call: {error:?}; stderr: {}", child.stop().1));
            let request =
                codec::read_request(&carried).expect("Storybook request decodes through the production codec");
            let reply = match request {
                Request::InterfaceRender { frame } => {
                    assert!(
                        frame.patches.len() <= PATCH_LIMIT,
                        "{story} loading frame emitted {} patches",
                        frame.patches.len()
                    );
                    rendered.push(frame);
                    Reply::Done
                }
                Request::InterfaceOpenTab { .. } => Reply::Identity("storybook-main".into()),
                Request::InterfaceRenderAt { slot, frame } => {
                    assert_eq!(slot, PRIMARY_SLOT);
                    assert!(
                        frame.patches.len() <= PATCH_LIMIT,
                        "{story} emitted {} patches",
                        frame.patches.len()
                    );
                    rendered.push(frame);
                    ready = true;
                    Reply::Done
                }
                Request::SourceResizeAt { .. } => Reply::Done,
                other => panic!("unexpected Storybook startup call: {other:?}"),
            };
            wire.send(&codec::reply(&reply).expect("reply encodes"))
                .expect("reply is sent");
            if ready {
                break;
            }
        }

        assert!(ready, "{story} never rendered");
        let mut tree = Tree::new();
        let mut surface = Surface::new();
        surface.theme(&Theme::dark()).expect("Storybook theme installs");
        for frame in rendered {
            tree.apply(&frame, &mut surface)
                .unwrap_or_else(|error| panic!("{story} failed in GTK: {error:?}"));
        }
        if story == "DataTable" {
            let length_deadline = Instant::now() + SOCKET_DEADLINE;
            loop {
                let carried =
                    receive_until(&mut wire, length_deadline).expect("DataTable publishes its logical length");
                if carried.kind == Kind::Credit {
                    continue;
                }
                let request = codec::read_request(&carried).expect("source length request decodes");
                let Request::SourceResizeAt { slot, mutation } = request else {
                    panic!("DataTable sent {request:?} before its source length")
                };
                assert_eq!(slot, PRIMARY_SLOT);
                let SourceMutation::Length { source, version, rows } = mutation else {
                    panic!("DataTable first source mutation was not its length")
                };
                assert_eq!(rows, 1_000_000);
                surface
                    .resize(source, version, rows)
                    .expect("GTK accepts logical source length");
                wire.send(&codec::reply(&Reply::Done).expect("done encodes"))
                    .expect("length acknowledgement sends");
                break;
            }
        }
        let root = surface.widget().clone().upcast::<gtk::Widget>();
        // A ColumnView realizes and recycles list-item children through a live
        // native root. Manually allocating it while unrooted exercises no valid
        // GTK lifecycle and leaves its factories measuring stale children.
        let realized_window = gtk::Window::new();
        let narrow_story = matches!(
            story,
            "Button"
                | "IconButton"
                | "Entry"
                | "Select"
                | "Checkbox"
                | "Radio"
                | "RadioGroup"
                | "FormControl"
                | "Slider"
                | "Heading"
                | "Expander"
                | "InlineMessage"
                | "RecoveryState"
                | "Switch"
                | "DataTable"
        );
        realized_window.set_default_size(if narrow_story { 600 } else { 1_200 }, 800);
        realized_window.set_child(Some(&root));
        realized_window.present();
        settle_toolkit();
        if story == "DataTable" {
            let titles = descendants::<gtk::Label>(&root)
                .into_iter()
                .filter(|label| {
                    label.has_css_class("hl-heading") && matches!(label.text().as_str(), "Data Table" | "DataTable")
                })
                .map(|label| label.text().to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                titles,
                ["Data Table"],
                "DataTable documentation must have one authoritative page title"
            );
        }
        if narrow_story {
            assert!(realized_window.width() <= 600, "{story} narrow capture remained wide");
            assert_contained(&root, story);
            if story == "RecoveryState" {
                assert_recovery_state(&root, "narrow");
            }
            capture_story(&realized_window, &format!("{story} narrow"));
            if story == "Button" {
                let document = descendants::<gtk::ScrolledWindow>(&root)
                    .into_iter()
                    .filter(|scroll| scroll.has_css_class("hl-scroll"))
                    .max_by(|left, right| left.vadjustment().upper().total_cmp(&right.vadjustment().upper()))
                    .expect("Button document owns a scrolling viewport");
                let horizontal = document.hadjustment();
                let wide_labels = descendants::<gtk::Label>(&root)
                    .into_iter()
                    .filter(|label| label.width() > 600)
                    .map(|label| {
                        (
                            label.text().to_string(),
                            label.width(),
                            label.measure(gtk::Orientation::Horizontal, -1),
                        )
                    })
                    .collect::<Vec<_>>();
                assert!(
                    horizontal.upper() <= horizontal.page_size() + 1.0,
                    "600px Button page widened to {}px for a {}px viewport; wide labels: {wide_labels:?}",
                    horizontal.upper(),
                    horizontal.page_size()
                );
                let allocated = descendants::<gtk::Button>(&root)
                    .into_iter()
                    .filter(|button| button.is_mapped() && !button.has_css_class("hl-listitembutton"))
                    .filter_map(|button| {
                        button_caption(&button)
                            .zip(button.compute_bounds(&root))
                            .map(|(caption, bounds)| (Some(caption), bounds))
                    })
                    .collect::<Vec<_>>();
                assert!(!allocated.is_empty(), "Button document has no mapped controls");
                for (caption, bounds) in &allocated {
                    assert!(
                        bounds.x() >= 16.0 && bounds.x() + bounds.width() <= 584.0,
                        "Button {caption:?} escaped 16px narrow insets: {bounds:?}"
                    );
                }
                for caption in ["Plain", "Warning"] {
                    assert!(
                        allocated.iter().any(|(label, bounds)| {
                            label.as_deref() == Some(caption) && bounds.x() + bounds.width() <= 584.0
                        }),
                        "rightmost {caption} specimen is absent or clipped"
                    );
                }
                for (class, expected) in [("size-small", 28), ("size-medium", 36), ("size-large", 44)] {
                    let heights = descendants::<gtk::Button>(&root)
                        .into_iter()
                        .filter(|button| button.has_css_class(class) && !button.has_css_class("hl-listitembutton"))
                        .map(|button| button.height())
                        .collect::<Vec<_>>();
                    assert!(
                        heights.iter().all(|height| *height == expected),
                        "narrow Button {class} specimens allocated {heights:?}, expected {expected}px"
                    );
                }
                capture_story(&realized_window, "Button narrow top");
                let vertical = document.vadjustment();
                let end = (vertical.upper() - vertical.page_size()).max(0.0);
                vertical.set_value(end / 2.0);
                settle_toolkit();
                capture_story(&realized_window, "Button narrow middle");
                vertical.set_value(end);
                settle_toolkit();
                assert!(
                    find::<gtk::Label>(&root, |label| label.text() == "API").height() > 0,
                    "Button API remains reachable at the end of the document"
                );
                assert!(
                    find::<gtk::Expander>(&root, |expander| { expander.label().as_deref() == Some("Playground") })
                        .height()
                        > 0,
                    "Button Playground remains reachable after API"
                );
                let property = find::<gtk::Label>(&root, |label| label.text() == "Property");
                let description = find::<gtk::Label>(&root, |label| label.text() == "Description");
                let row = property
                    .parent()
                    .and_then(|parent| parent.downcast::<gtk::Box>().ok())
                    .expect("Button API header is a table row");
                assert!(
                    !row.is_homogeneous(),
                    "authored API column widths were replaced with equal shares"
                );
                assert!(
                    description.width() > property.width(),
                    "narrow API description remained a word-wide tower: property={}, description={}",
                    property.width(),
                    description.width()
                );
                let table = row
                    .parent()
                    .and_then(|head| head.parent())
                    .expect("Button API header belongs to a table");
                let rows = descendants::<gtk::Box>(&table)
                    .into_iter()
                    .filter(|candidate| candidate.has_css_class("hl-tablerow"))
                    .collect::<Vec<_>>();
                let positions = |candidate: &gtk::Box| {
                    let mut positions = Vec::new();
                    let mut child = candidate.first_child();
                    while let Some(cell) = child {
                        child = cell.next_sibling();
                        positions.push(cell.allocation().x());
                    }
                    positions
                };
                let columns = positions(rows.first().expect("Button API table has a header"));
                let row_columns = rows.iter().map(positions).collect::<Vec<_>>();
                assert!(
                    row_columns.iter().all(|candidate| candidate == &columns),
                    "authored API columns did not remain aligned across rows: {row_columns:?}"
                );
                capture_story(&realized_window, "Button narrow bottom");
                vertical.set_value(0.0);
                settle_toolkit();
            }
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
        }
        if story == "Button" {
            let search = find::<gtk::Entry>(&root, |entry| {
                entry.placeholder_text().as_deref() == Some("Search components")
            });
            let width = search.width();
            search.set_text("Se");
            allocate(&root, 1_200, 800);
            settle_toolkit();
            assert!(width >= 200, "Storybook search started at only {width}px");
            assert_eq!(
                search.width(),
                width,
                "a short query collapsed the Storybook search field"
            );
            let _ = surface.reports().drain();
        }
        if story == "Button" {
            for (class, expected) in [("size-small", 28), ("size-medium", 36), ("size-large", 44)] {
                let heights = descendants::<gtk::Button>(&root)
                    .into_iter()
                    .filter(|button| button.has_css_class(class) && !button.has_css_class("hl-listitembutton"))
                    .map(|button| button.height())
                    .collect::<Vec<_>>();
                assert!(!heights.is_empty(), "Button has no {class} specimens");
                assert!(
                    heights.iter().all(|height| *height == expected),
                    "Button {class} specimens allocated {heights:?}, expected {expected}px"
                );
            }
        }
        if story == "IconButton" {
            for (class, expected, icon) in [("size-small", 28, 14), ("size-medium", 36, 18), ("size-large", 44, 20)] {
                let sizes = descendants::<gtk::Button>(&root)
                    .into_iter()
                    .filter(|button| button.has_css_class(class))
                    .map(|button| (button.width(), button.height()))
                    .collect::<Vec<_>>();
                assert!(!sizes.is_empty(), "IconButton has no {class} specimens");
                assert!(
                    sizes.iter().all(|size| *size == (expected, expected)),
                    "IconButton {class} specimens allocated {sizes:?}, expected {expected}px square"
                );
                let icon_sizes = descendants::<gtk::Button>(&root)
                    .into_iter()
                    .filter(|button| button.has_css_class(class))
                    .filter_map(|button| button.child())
                    .filter_map(|child| child.downcast::<gtk::Image>().ok())
                    .map(|image| (image.width(), image.height()))
                    .collect::<Vec<_>>();
                assert!(!icon_sizes.is_empty(), "IconButton has no {class} icon specimen");
                assert!(
                    icon_sizes.iter().all(|size| size.0 >= icon && size.1 == icon),
                    "IconButton {class} icon paint boxes {icon_sizes:?} did not preserve {icon}px optical size"
                );
            }
            let fallback = find::<gtk::Button>(&root, |button| button.tooltip_text().as_deref() == Some("Refresh"));
            assert_eq!(fallback.icon_name().as_deref(), Some("view-refresh-symbolic"));
            let override_ = find::<gtk::Button>(&root, |button| {
                button.tooltip_text().as_deref() == Some("Use the host default for font size")
            });
            assert_eq!(override_.icon_name().as_deref(), Some("edit-clear-symbolic"));
            let focus = find::<gtk::Button>(&root, |button| {
                button.tooltip_text().as_deref() == Some("Keyboard focus")
            });
            assert!(focus.grab_focus(), "IconButton accepts deterministic keyboard focus");
            settle_toolkit();
            assert!(focus.has_focus(), "IconButton exposes its native focus state");
            let _ = surface.reports().drain();
        }
        if story == "Entry" {
            let focus = find::<gtk::Entry>(&root, |entry| {
                entry.tooltip_text().as_deref() == Some("Focused extension name")
            });
            let guidance = find::<gtk::Label>(&root, |label| {
                label.text() == "Choose width from expected content, not from the current value."
            });
            let guidance_color = guidance.color();
            assert_eq!(guidance.accessible_role(), gtk::AccessibleRole::Label);
            assert!((14..=18).contains(&guidance.height()));
            assert_eq!(
                [
                    (guidance_color.red() * 255.0).round() as u8,
                    (guidance_color.green() * 255.0).round() as u8,
                    (guidance_color.blue() * 255.0).round() as u8,
                ],
                [0xbe, 0xc5, 0xcf],
                "Text color=text-dim resolves the shared AAA secondary text token"
            );
            assert!(focus.grab_focus(), "Entry accepts deterministic keyboard focus");
            settle_toolkit();
            assert_document_horizontally_contained(&root, "focused Entry");
            let _ = surface.reports().drain();
        }
        if story == "FormControl" {
            let entry = find::<gtk::Entry>(&root, |entry| entry.tooltip_text().as_deref() == Some("Extension name"));
            let label = find::<gtk::Label>(&root, |label| label.text() == "Extension name");
            let helper = find::<gtk::Label>(&root, |label| label.text() == "Used in manifests and package names.");
            assert_eq!(
                label.mnemonic_widget(),
                Some(entry.clone().upcast()),
                "FormLabel identifies its sibling Entry"
            );
            assert_eq!(helper.accessible_role(), gtk::AccessibleRole::Label);
            assert!(helper.is_visible(), "FormHelperText remains visible beside its Entry");
            assert!(
                (14..=18).contains(&helper.height()),
                "FormHelperText is {}px tall",
                helper.height()
            );
            assert!(entry.grab_focus(), "FormControl Entry accepts deterministic focus");
            settle_toolkit();
            let _ = surface.reports().drain();
        }
        if story == "Checkbox" {
            let checkbox =
                find::<gtk::CheckButton>(&root, |button| button.label().as_deref() == Some("Select all · 1 of 3"));
            assert!(checkbox.is_inconsistent(), "parent Checkbox exposes native mixed state");
            assert!(checkbox.grab_focus(), "mixed Checkbox accepts native keyboard focus");
            settle_toolkit();
            assert_eq!(checkbox.accessible_role(), gtk::AccessibleRole::Checkbox);
            assert_eq!(checkbox.height(), 18, "Checkbox keeps its compact 16px indicator row");
            assert!(
                checkbox.width() >= 900,
                "wide Checkbox fixture did not exercise an expanding row: {}px",
                checkbox.width()
            );
            capture_story(&realized_window, "Checkbox focused mixed");
            let _ = surface.reports().drain();
        }
        if story == "Slider" {
            let slider = find::<gtk::Scale>(&root, |scale| {
                scale.tooltip_text().as_deref() == Some("Build cache allocation")
            });
            let disclosure = find::<gtk::Expander>(&root, |expander| expander.label().as_deref() == Some("Show code"));
            assert!(!disclosure.is_expanded(), "Slider code starts collapsed");
            assert!(
                find::<gtk::Label>(&root, |label| label.text() == "API").is_visible(),
                "compact Slider keeps the API heading in its first wide viewport"
            );
            assert_eq!(slider.adjustment().lower(), 0.0);
            assert_eq!(slider.adjustment().upper(), 100.0);
            assert_eq!(slider.adjustment().step_increment(), 5.0);
            assert_eq!(slider.value(), 40.0);
            assert!(slider.grab_focus(), "Slider accepts deterministic keyboard focus");
            settle_toolkit();
        }
        if story == "Switch" {
            let focus = labelled_switch(&root, "Restore panes on launch");
            let caption = find::<gtk::Label>(&root, |label| label.text() == "Restore panes on launch");
            assert_eq!(focus.accessible_role(), gtk::AccessibleRole::Switch);
            assert_eq!(
                (focus.width(), focus.height()),
                (44, 22),
                "Switch keeps a compact exact track geometry inside its 44px label row"
            );
            assert!(focus.is_active(), "Switch story begins in its authored on state");
            assert_eq!(
                focus.tooltip_text(),
                None,
                "visible caption is not duplicated as a tooltip"
            );
            assert_eq!(caption.mnemonic_widget(), Some(focus.clone().upcast()));
            let row = caption.parent().expect("Switch caption remains in its label row");
            assert!(row.height() >= 44, "Switch label row allocated only {}px", row.height());
            let gesture = row
                .observe_controllers()
                .into_iter()
                .flatten()
                .find_map(|controller| controller.downcast::<gtk::GestureClick>().ok())
                .expect("Switch label row owns pointer activation");
            let caption_bounds = caption.compute_bounds(&row).expect("caption is allocated in its row");
            gesture.emit_by_name::<()>(
                "released",
                &[
                    &1_i32,
                    &f64::from(caption_bounds.center().x()),
                    &f64::from(caption_bounds.center().y()),
                ],
            );
            settle_toolkit();
            assert!(
                matches!(
                    surface.reports().drain().as_slice(),
                    [hl_gui::Event::Toggle {
                        value: hl_gui::PropValue::Flag(false),
                        ..
                    }]
                ),
                "caption click reports exactly one request for the controlled off state"
            );
            assert!(focus.grab_focus(), "Switch accepts deterministic keyboard focus");
            let keyboard = focus
                .observe_controllers()
                .into_iter()
                .flatten()
                .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
                .expect("Switch owns Enter activation");
            keyboard.emit_by_name::<bool>(
                "key-pressed",
                &[&gtk::gdk::Key::Return, &36_u32, &gtk::gdk::ModifierType::empty()],
            );
            settle_toolkit();
            assert!(
                matches!(
                    surface.reports().drain().as_slice(),
                    [hl_gui::Event::Toggle {
                        value: hl_gui::PropValue::Flag(false),
                        ..
                    }]
                ),
                "Enter reports exactly one request for the controlled off state"
            );
            keyboard.emit_by_name::<bool>(
                "key-pressed",
                &[&gtk::gdk::Key::space, &65_u32, &gtk::gdk::ModifierType::empty()],
            );
            settle_toolkit();
            assert!(
                matches!(
                    surface.reports().drain().as_slice(),
                    [hl_gui::Event::Toggle {
                        value: hl_gui::PropValue::Flag(false),
                        ..
                    }]
                ),
                "Space reports exactly one request for the controlled off state"
            );
            assert!(focus.is_active(), "controlled Switch waits for its next authored frame");
            focus.set_state_flags(gtk::StateFlags::FOCUSED, false);
            settle_toolkit();
            capture_story(&realized_window, "Switch focused on");
        }
        if story == "Select" {
            let focus = find::<gtk::ToggleButton>(&root, |button| {
                button.tooltip_text().as_deref() == Some("Focused shell selector")
            });
            assert!(focus.grab_focus(), "Select accepts deterministic keyboard focus");
            settle_toolkit();
            let _ = surface.reports().drain();
            let live = find::<gtk::ToggleButton>(&root, |button| {
                button.tooltip_text().as_deref() == Some("Default shell")
            });
            live.emit_clicked();
            settle_toolkit();
            let popover = find::<gtk::Popover>(&live.clone().upcast(), |_| true);
            assert!(
                popover.is_visible(),
                "Select open-list specimen did not reveal its options"
            );
            let options = find::<gtk::Box>(&popover.clone().upcast(), |options| {
                options.has_css_class("hl-select-options")
            });
            assert_eq!(options.accessible_role(), gtk::AccessibleRole::ListBox);
            assert_eq!(live.accessible_role(), gtk::AccessibleRole::ComboBox);
            assert_eq!(options.spacing(), 2);
            let option_buttons = descendants::<gtk::Button>(options.upcast_ref())
                .into_iter()
                .filter(|button| button.has_css_class("hl-select-option"))
                .collect::<Vec<_>>();
            assert_eq!(
                option_buttons.len(),
                3,
                "Select popup keeps one native option per choice"
            );
            let option_metrics = option_buttons
                .iter()
                .map(|button| {
                    (
                        button.accessible_role(),
                        button.measure(gtk::Orientation::Horizontal, -1).0,
                        button.measure(gtk::Orientation::Vertical, -1).0,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                option_metrics,
                vec![(gtk::AccessibleRole::Option, 142, 28); 3],
                "Select options keep exact role and compact natural geometry"
            );
            let selected = option_buttons
                .iter()
                .find(|button| button.label().as_deref() == Some("Z shell"))
                .expect("current Select value remains present in its popup");
            assert!(selected.has_css_class("selected"));
            let focused = option_buttons
                .iter()
                .find(|button| button.label().as_deref() == Some("Bash"))
                .expect("Select popup exposes its adjacent focus target");
            assert!(focused.grab_focus(), "Select option accepts native keyboard focus");
            focused.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            settle_toolkit();
            assert!(focused.has_focus());
            assert!(focused.state_flags().contains(gtk::StateFlags::PRELIGHT));
            assert!(
                !focused.has_css_class("selected"),
                "keyboard focus must not impersonate the current Select value"
            );
            capture_story(&realized_window, "Select open");
            capture_widget(
                &realized_window,
                popover.upcast_ref::<gtk::Widget>(),
                "Select open list",
            );
            live.emit_clicked();
            settle_toolkit();
        }
        if story == "Search" {
            let search = find::<gtk::SearchEntry>(&root, |entry| {
                entry.tooltip_text().as_deref() == Some("Find extensions")
            });
            assert_eq!(search.accessible_role(), gtk::AccessibleRole::SearchBox);
            assert_eq!(search.placeholder_text().as_deref(), Some("Name, status, or provider"));
            assert!(
                (28..=36).contains(&search.height()),
                "Search is {}px tall",
                search.height()
            );
            assert!(search.grab_focus(), "Search accepts keyboard focus");
            let focus =
                gtk::prelude::RootExt::focus(&realized_window).expect("Search delegates focus to its native editable");
            assert!(
                focus == search.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&search),
                "Search does not own the focused native editable"
            );
            capture_story(&realized_window, "Search before wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "Search before narrow");
            assert!(search.width() <= 552, "Search escaped 16px narrow insets");
            capture_story(&realized_window, "Search before narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
        }
        if story == "TextArea" {
            let editor = find::<gtk::ScrolledWindow>(&root, |window| {
                window.tooltip_text().as_deref() == Some("Task manifest")
            });
            let view = editor
                .child()
                .and_then(|child| child.downcast::<gtk::TextView>().ok())
                .expect("TextArea owns a native multi-line editor");
            assert_eq!(view.accessible_role(), gtk::AccessibleRole::TextBox);
            assert!(view.is_editable());
            assert!(view.is_monospace());
            assert_eq!(view.wrap_mode(), gtk::WrapMode::WordChar);
            assert!(
                (104..=120).contains(&editor.height()),
                "TextArea is {}px tall",
                editor.height()
            );
            assert!(view.grab_focus(), "TextArea accepts keyboard focus");
            capture_story(&realized_window, "TextArea before wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "TextArea before narrow");
            assert!(editor.width() <= 552);
            capture_story(&realized_window, "TextArea before narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
        }
        if story == "NumberEntry" {
            let counter =
                find::<gtk::SpinButton>(&root, |spin| spin.tooltip_text().as_deref() == Some("Build workers"));
            assert_eq!(counter.accessible_role(), gtk::AccessibleRole::SpinButton);
            assert_eq!(counter.value(), 4.0);
            assert_eq!(counter.adjustment().lower(), 1.0);
            assert_eq!(counter.adjustment().upper(), 16.0);
            assert_eq!(counter.adjustment().step_increment(), 1.0);
            assert!((28..=36).contains(&counter.height()));
            let fractional = find::<gtk::SpinButton>(&root, |spin| {
                (spin.adjustment().step_increment() - 0.25).abs() < f64::EPSILON
            });
            assert_eq!(fractional.digits(), 2);
            assert_eq!(fractional.value(), 1.5);
            assert_eq!(fractional.text(), "1.50");
            assert!(counter.grab_focus(), "NumberEntry accepts keyboard focus");
            capture_story(&realized_window, "NumberEntry before wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "NumberEntry before narrow");
            assert!(counter.width() <= 196, "NumberEntry expanded to {}px", counter.width());
            capture_story(&realized_window, "NumberEntry before narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
        }
        if story == "PasswordEntry" {
            let field =
                find::<gtk::PasswordEntry>(&root, |entry| entry.tooltip_text().as_deref() == Some("Registry token"));
            assert_eq!(field.accessible_role(), gtk::AccessibleRole::TextBox);
            assert_eq!(field.text(), "local-token");
            assert!(field.shows_peek_icon(), "documented reveal policy exposes native peek");
            assert!((28..=36).contains(&field.height()));
            let withheld = find::<gtk::PasswordEntry>(&root, |entry| entry.text() == "never-reveal");
            assert!(!withheld.shows_peek_icon(), "secret=true withholds native peek");
            assert!(field.grab_focus(), "PasswordEntry accepts keyboard focus");
            capture_story(&realized_window, "PasswordEntry before wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "PasswordEntry before narrow");
            assert!(field.width() <= 360);
            capture_story(&realized_window, "PasswordEntry before narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
        }
        if story == "Autocomplete" {
            let choice = find::<gtk::DropDown>(&root, |drop| drop.tooltip_text().as_deref() == Some("Runtime"));
            assert_eq!(choice.accessible_role(), gtk::AccessibleRole::ComboBox);
            assert!(choice.enables_search());
            assert_eq!(choice.model().expect("Autocomplete options").n_items(), 3);
            assert!(
                (28..=44).contains(&choice.height()),
                "Autocomplete is {}px tall",
                choice.height()
            );
            assert!(choice.grab_focus(), "Autocomplete accepts keyboard focus");
            capture_story(&realized_window, "Autocomplete before wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "Autocomplete before narrow");
            assert!(choice.width() <= 360);
            capture_story(&realized_window, "Autocomplete before narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
        }
        let toggle_before = if story == "ToggleButton" {
            let toggle =
                find::<gtk::ToggleButton>(&root, |button| button.tooltip_text().as_deref() == Some("Pin this tab"));
            let label = find::<gtk::Label>(&toggle.clone().upcast(), |label| label.text() == "Pin tab");
            assert!(
                (28..=44).contains(&toggle.height()),
                "documented ToggleButton allocated {}px instead of a compact control height",
                toggle.height()
            );
            let label_center = label.allocation().y() + label.height() / 2;
            assert!(
                (label_center - toggle.height() / 2).abs() <= 1,
                "documented ToggleButton label center {label_center}px is not centered in {}px",
                toggle.height()
            );
            assert_eq!(toggle.accessible_role(), gtk::AccessibleRole::ToggleButton);
            assert_toggle_receipt(&root, "No change yet.", 36);
            capture_story(&realized_window, "ToggleButton before wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "ToggleButton narrow");
            assert_toggle_receipt(&root, "No change yet.", 56);
            capture_story(&realized_window, "ToggleButton before narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
            Some(toggle.is_active())
        } else {
            None
        };
        if matches!(story, "Button" | "Entry" | "Heading" | "DataTable") {
            let paned = descendants::<gtk::Paned>(&root)
                .into_iter()
                .next()
                .expect("wide component document owns a responsive pane");
            assert!(
                (238..=242).contains(&paned.position()),
                "{story} relayout collapsed its authored 240px navigation to {}px",
                paned.position(),
            );
            assert_document_horizontally_contained(&root, story);
        }
        if story == "RecoveryState" {
            assert_recovery_state(&root, "wide");
        }
        if story == "Navigation and transient UI" {
            let mut menu_items = descendants::<gtk::Button>(&root)
                .into_iter()
                .filter(|button| button.has_css_class("hl-menu-item"))
                .collect::<Vec<_>>();
            menu_items.reverse();
            assert_eq!(
                menu_items.len(),
                2,
                "command palette retains both described menu actions"
            );
            let captions = menu_items
                .iter()
                .flat_map(|button| descendants::<gtk::Label>(&button.clone().upcast()))
                .map(|label| label.text().to_string())
                .collect::<Vec<_>>();
            assert_eq!(captions, ["Open terminal", "Create workspace"]);
            let terminal_icon = descendants::<gtk::Image>(&menu_items[0].clone().upcast())
                .into_iter()
                .next()
                .expect("terminal menu action retains its icon slot");
            let icon_theme = gtk::IconTheme::for_display(&terminal_icon.display());
            if icon_theme.has_icon("utilities-terminal-symbolic") {
                assert_eq!(
                    terminal_icon.icon_name().as_deref(),
                    Some("utilities-terminal-symbolic")
                );
            } else if icon_theme.has_icon("application-x-executable-symbolic") {
                assert_eq!(
                    terminal_icon.icon_name().as_deref(),
                    Some("application-x-executable-symbolic")
                );
            } else {
                let terminal_names = terminal_icon
                    .gicon()
                    .and_then(|icon| icon.downcast::<gtk::gio::ThemedIcon>().ok())
                    .expect("extension icons resolve through a themed fallback chain")
                    .names();
                assert_eq!(
                    terminal_names.first().map(|name| name.as_str()),
                    Some("utilities-terminal-symbolic")
                );
                assert!(
                    terminal_names.iter().any(|name| name == "view-more-symbolic"),
                    "a host missing the preferred terminal icon must render a portable fallback: {terminal_names:?}"
                );
            }
            let wide_metrics = menu_items
                .iter()
                .map(|button| (button.accessible_role(), button.width(), button.height()))
                .collect::<Vec<_>>();
            assert_eq!(
                wide_metrics,
                vec![(gtk::AccessibleRole::MenuItem, 902, 28); 2],
                "menu actions keep native roles and exact compact wide geometry"
            );
            assert!(menu_items[0].grab_focus(), "first menu action accepts keyboard focus");
            menu_items[0].set_state_flags(gtk::StateFlags::PRELIGHT, false);
            settle_toolkit();
            assert!(menu_items[0].has_focus());
            assert!(menu_items[0].state_flags().contains(gtk::StateFlags::PRELIGHT));
            capture_story(&realized_window, "Navigation menu wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "Navigation menu narrow");
            let narrow_metrics = menu_items
                .iter()
                .map(|button| (button.width(), button.height()))
                .collect::<Vec<_>>();
            assert_eq!(narrow_metrics, vec![(550, 28); 2]);
            capture_story(&realized_window, "Navigation menu narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
        }
        capture_story(&realized_window, story);
        if story == "Heading" {
            let specimens = descendants::<gtk::Label>(&root)
                .into_iter()
                .filter(|label| label.text() == "Build, inspect, and ship with confidence")
                .collect::<Vec<_>>();
            assert_eq!(specimens.len(), 4, "Heading renders every semantic scale exactly once");
            for class in ["scale-caption", "scale-body", "scale-title", "scale-display"] {
                assert!(
                    specimens.iter().any(|label| label.has_css_class(class)),
                    "Heading specimen omitted {class}"
                );
            }
            for label in specimens {
                assert_eq!(label.accessible_role(), gtk::AccessibleRole::Heading);
            }
        }
        if story == "Expander" {
            let controlled = find::<gtk::Expander>(&root, |expander| {
                expander.label().as_deref() == Some("Runtime diagnostics")
            });
            assert_eq!(controlled.label().as_deref(), Some("Runtime diagnostics"));
            assert_eq!(controlled.accessible_role(), gtk::AccessibleRole::Button);
            assert!(controlled.has_css_class("hl-expander"));
            assert!(!controlled.is_expanded(), "controlled Expander starts collapsed");
            assert_eq!(
                controlled.height(),
                28,
                "collapsed disclosure owns one compact control row"
            );
            assert!(
                controlled.can_focus(),
                "the labelled disclosure participates in keyboard focus"
            );
            assert!(controlled.grab_focus(), "Expander summary owns keyboard focus");
            settle_toolkit();
            assert!(controlled.state_flags().contains(gtk::StateFlags::FOCUSED));
            capture_story(&realized_window, "Expander focused collapsed");
            // GTK's native Enter and Space bindings both dispatch this action signal.
            // Exercising it in both directions proves each binding's common path
            // changes state and reports once without installing a synthetic handler.
            for expected in [true, false] {
                controlled.emit_by_name::<()>("activate", &[]);
                settle_toolkit();
                allocate(&root, realized_window.width(), 1_600);
                realized_window.queue_draw();
                settle_toolkit();
                let reports = surface.reports().drain();
                assert_eq!(reports.len(), 1, "one keyboard activation reports exactly once");
                let hl_gui::Event::Expand { value, .. } = &reports[0] else {
                    panic!("keyboard activation emitted the wrong report: {:?}", reports[0])
                };
                assert_eq!(value, &hl_gui::PropValue::Flag(expected));
                assert_eq!(controlled.is_expanded(), expected);
                assert!(controlled.has_focus(), "toggle preserves focus on the summary");
                if expected {
                    assert!(
                        controlled.height() > 28,
                        "expanded disclosure allocates its body below the 28px summary"
                    );
                    capture_story(&realized_window, "Expander focused expanded");
                }
            }

            let mut states = descendants::<gtk::Expander>(&root)
                .into_iter()
                .filter(|expander| expander.label().as_deref() == Some("Technical details"))
                .map(|expander| expander.is_expanded())
                .collect::<Vec<_>>();
            states.sort_unstable();
            assert_eq!(states, [false, true], "Expander shows both canonical disclosure states");

            let bounded = find::<gtk::Expander>(&root, |expander| {
                expander
                    .label()
                    .is_some_and(|label| label.starts_with("Connection diagnostics"))
            });
            assert!(
                bounded.width() <= root.width(),
                "long Expander allocated {}px beyond its {}px page",
                bounded.width(),
                root.width()
            );
        }
        if story == "Checkbox" {
            let property = find::<gtk::Label>(&root, |label| label.text() == "Property");
            let owned = ["label", "checked", "indeterminate", "enabled", "onToggle"]
                .map(|name| find::<gtk::Label>(&root, |label| label.text() == name));
            assert!(
                owned.iter().all(|label| label.height() > 0),
                "Checkbox API owned rows receive real GTK allocations"
            );
            let table = property
                .ancestor(gtk::ScrolledWindow::static_type())
                .and_then(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
                .expect("Checkbox API header belongs to its native table scroller");
            assert!(
                table.parent().is_none_or(|parent| !parent.is::<gtk::ScrolledWindow>()),
                "Checkbox API table must not be collapsed inside a second scroller"
            );
            let document = descendants::<gtk::ScrolledWindow>(&root)
                .into_iter()
                .max_by(|left, right| left.vadjustment().upper().total_cmp(&right.vadjustment().upper()))
                .expect("Checkbox document owns a scrolling viewport");
            let adjustment = document.vadjustment();
            adjustment.set_value(adjustment.upper() - adjustment.page_size());
            settle_toolkit();
            capture_story(&realized_window, "Checkbox API wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "Checkbox API narrow");
            capture_story(&realized_window, "Checkbox API narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            settle_window_width(&realized_window, 1_200);
            adjustment.set_value(0.0);
            settle_toolkit();
        }
        let responsive = matches!(story, "Button" | "IconButton" | "DataTable").then(|| {
            let paned = descendants::<gtk::Paned>(&root)
                .into_iter()
                .next()
                .expect("component documentation owns a responsive shell");
            let body = paned.end_child().expect("wide responsive shell owns the document body");
            (paned, body)
        });
        for width in [600, 1_200] {
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 1_600, -1, None);
            assert_eq!(root.width(), width, "{story} did not accept the {width}px allocation");
            assert_contained(&root, story);
            if let Some((paned, body)) = &responsive {
                let layout = paned.parent().expect("responsive paned remains in its layout");
                let compact = layout.first_child().expect("responsive shell keeps compact navigation");
                if width == 600 {
                    assert!(!paned.is_visible(), "{story} left its desktop sidebar visible at 600px");
                    assert!(compact.is_visible(), "{story} hid its compact selector at 600px");
                    assert!(
                        layout.last_child().is_some_and(|child| child.eq(body)),
                        "{story} did not give the shared document the compact width"
                    );
                } else {
                    assert!(
                        paned.is_visible(),
                        "{story} did not restore its desktop sidebar at 1200px"
                    );
                    assert!(
                        !compact.is_visible(),
                        "{story} kept duplicate compact navigation at 1200px"
                    );
                    assert!(
                        paned.end_child().is_some_and(|child| child.eq(body)),
                        "{story} rebuilt or lost its document while changing responsive branches"
                    );
                    let navigation = paned.start_child().expect("wide shell retains navigation");
                    let body = paned.end_child().expect("wide shell retains its document");
                    assert_eq!(paned.accessible_role(), gtk::AccessibleRole::Separator);
                    assert!(paned.is_focusable(), "responsive divider is keyboard reachable");
                    assert!(
                        paned.has_css_class("hl-responsive-divider"),
                        "responsive divider exposes its shared interaction chrome"
                    );
                    let navigation_bounds = navigation
                        .compute_bounds(paned)
                        .expect("navigation belongs to the responsive divider");
                    let body_bounds = body
                        .compute_bounds(paned)
                        .expect("document belongs to the responsive divider");
                    assert_eq!(
                        (body_bounds.x() - navigation_bounds.x() - navigation_bounds.width()).round(),
                        8.0,
                        "{story} responsive divider must expose an exact 8px interaction target"
                    );
                    assert!(
                        (238..=242).contains(&navigation.width()),
                        "{story} allocated {}px to its authored 240px navigation",
                        navigation.width(),
                    );
                    let navigation_selects = descendants::<gtk::ToggleButton>(&navigation)
                        .into_iter()
                        .filter(|button| button.has_css_class("hl-select"))
                        .collect::<Vec<_>>();
                    assert_eq!(
                        navigation_selects.len(),
                        2,
                        "{story} navigation keeps mode and family selectors"
                    );
                    assert!(
                        navigation_selects.iter().all(|select| select.width() >= 200),
                        "{story} navigation selectors did not consume the pane: {:?}",
                        navigation_selects
                            .iter()
                            .map(|select| select.width())
                            .collect::<Vec<_>>(),
                    );
                }
            }
            if story == "DataTable" && width == 1_200 {
                let panes = descendants::<gtk::ScrolledWindow>(&root)
                    .into_iter()
                    .filter(|scroll| scroll.has_css_class("hl-scroll"))
                    .collect::<Vec<_>>();
                assert_eq!(
                    panes.len(),
                    2,
                    "Storybook keeps navigation and document panes without nesting API table scrollers",
                );
                assert!(
                    panes.iter().all(|pane| pane.width() >= 150),
                    "wide Storybook panes were clipped to {:?}",
                    panes.iter().map(|pane| pane.width()).collect::<Vec<_>>()
                );
            }
        }
        if story == "Button" {
            let paned = descendants::<gtk::Paned>(&root)
                .into_iter()
                .next()
                .expect("Button documentation owns desktop navigation");
            let example = find::<gtk::Label>(&root, |label| {
                label.has_css_class("hl-code") && label.text().starts_with("<Button")
            });
            let (_, text_height) = example.layout().pixel_size();
            let (_, natural_height, _, _) = example.measure(gtk::Orientation::Vertical, example.width());
            assert!(
                natural_height >= text_height + 16,
                "source example omitted its compact 8px vertical inset: text={text_height}, natural={natural_height}"
            );
            let navigation = paned.start_child().expect("wide Button documentation retains its rail");
            let navigation_headers = descendants::<gtk::Label>(&navigation)
                .into_iter()
                .filter(|label| label.has_css_class("hl-listsubheader"))
                .collect::<Vec<_>>();
            assert_eq!(
                navigation_headers.len(),
                4,
                "Storybook renders one semantic heading for each visible navigation group"
            );
            let mut header_labels = navigation_headers
                .iter()
                .map(|header| header.text().to_string())
                .collect::<Vec<_>>();
            header_labels.sort();
            assert_eq!(header_labels, ["Browse", "Buttons", "Component family", "Components"]);
            for header in &navigation_headers {
                assert_eq!(header.accessible_role(), gtk::AccessibleRole::Heading);
                assert_eq!(header.height(), 16, "Storybook group headings remain compact");
                let description = header
                    .pango_context()
                    .font_description()
                    .expect("Storybook group heading has computed typography");
                assert_eq!(description.size(), 11 * gtk::pango::SCALE);
                assert_eq!(description.weight(), gtk::pango::Weight::Semibold);
                let bounds = header
                    .compute_bounds(&navigation)
                    .expect("Storybook group heading belongs to the desktop rail");
                assert!(
                    bounds.x() >= 4.0 && bounds.x() + bounds.width() <= (navigation.width() - 4) as f32,
                    "Storybook group heading escaped the rail's clipping-safe inset: {bounds:?}"
                );
            }
            let destinations = descendants::<gtk::Button>(&navigation)
                .into_iter()
                .filter(|button| button.has_css_class("hl-listitembutton"))
                .collect::<Vec<_>>();
            assert_eq!(
                destinations.len(),
                11,
                "the Buttons family remains one bounded component list"
            );
            let heights = destinations.iter().map(|button| button.height()).collect::<Vec<_>>();
            assert!(
                heights.iter().all(|height| *height == 26),
                "Storybook component rows were not exactly 26px: {heights:?}"
            );
            for destination in &destinations {
                assert_eq!(destination.accessible_role(), gtk::AccessibleRole::Button);
                assert!(
                    destination.is_focusable(),
                    "every Storybook component is keyboard reachable"
                );
                let bounds = destination
                    .compute_bounds(&navigation)
                    .expect("component destination belongs to the Storybook rail");
                assert!(
                    bounds.x() >= 4.0 && bounds.x() + bounds.width() <= (navigation.width() - 4) as f32,
                    "component destination escaped the rail's clipping-safe inset: {bounds:?}"
                );
            }
            let selected = destinations
                .iter()
                .find(|button| button_caption(button).as_deref() == Some("Button"))
                .expect("Button is the selected component");
            let hovered = destinations
                .iter()
                .find(|button| button_caption(button).as_deref() == Some("IconButton"))
                .expect("IconButton is the adjacent component");
            assert!(selected.has_css_class("variant-filled"));
            assert!(hovered.has_css_class("variant-ghost"));
            assert!(selected.grab_focus(), "selected component accepts keyboard focus");
            hovered.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            settle_toolkit();
            assert!(selected.has_focus(), "selected component owns the native focus state");
            assert!(
                hovered.state_flags().contains(gtk::StateFlags::PRELIGHT),
                "adjacent component exposes the native prelight state"
            );
            capture_story(&realized_window, "Button navigation states");
            let body = paned.end_child().expect("Button document remains beside navigation");
            let body_before = body.width();
            paned.set_position(280);
            root.allocate(1_200, 1_600, -1, None);
            settle_toolkit();
            assert_eq!(navigation.width(), 280, "native divider resizes the Storybook rail");
            assert_eq!(
                body.width(),
                body_before - 40,
                "resizing the rail transfers exactly the same width from the document"
            );
            assert!(paned.grab_focus(), "resized divider accepts keyboard focus");
            assert!(paned.has_focus(), "resized divider exposes its focused handle state");
            capture_story(&realized_window, "Button resized navigation");
            paned.set_position(240);
            root.allocate(1_200, 1_600, -1, None);
            selected.grab_focus();
            settle_toolkit();
        }
        if story == "DataTable" {
            settle_toolkit();
            let requests = surface.requests(1);
            assert!(!requests.is_empty(), "realized GTK rows request a source window");
            let request = requests[0].clone();
            assert!(request.range.count <= 128, "GTK requested an unbounded row window");
            let channel = ChannelId::new(4);
            wire.send(&Frame::new(
                channel,
                Kind::Event,
                serde_json::to_vec(&request).expect("row request encodes"),
            ))
            .expect("row request reaches Storybook");
            let row_deadline = Instant::now() + SOCKET_DEADLINE;
            let answer = loop {
                let carried = receive_until(&mut wire, row_deadline).unwrap_or_else(|error| {
                    let diagnostic = child.stop().1;
                    panic!("Storybook answers the row request: {error:?}; stderr: {diagnostic}")
                });
                if carried.kind == Kind::Credit {
                    continue;
                }
                if carried.kind == Kind::Response && carried.channel == channel {
                    break carried;
                }
                let follow_up = codec::read_request(&carried).expect("concurrent Storybook call decodes");
                apply_concurrent(&follow_up, &mut tree, &mut surface);
                wire.send(&codec::reply(&Reply::Done).expect("follow-up reply encodes"))
                    .expect("follow-up reply sends");
            };
            assert_eq!(answer.channel, channel);
            assert_eq!(answer.kind, Kind::Response);
            let window: hl_gui::RowWindow = serde_json::from_slice(&answer.payload).expect("row window decodes");
            assert!(
                window.rows.len() <= 128,
                "Storybook materialized an unbounded row window"
            );
            surface.rows(&window).expect("GTK accepts the bounded row window");
            settle_toolkit();
            let view = find::<gtk::ColumnView>(&root, |_| true);
            let table = view
                .ancestor(gtk::ScrolledWindow::static_type())
                .and_then(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
                .expect("DataTable ColumnView remains inside its scrolling viewport");
            assert!(
                (318..=322).contains(&table.height()),
                "authored step80 DataTable allocated {}px instead of 320px",
                table.height(),
            );
            let model = view.model().expect("ready DataTable keeps a selection model");
            assert!(model.select_item(0, true), "ready DataTable selects a visible row");
            settle_toolkit();
            let selection = surface
                .reports()
                .drain()
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Select { .. }))
                .expect("native row selection produces a typed event");
            let payload = codec::interaction(&selection, Some(PRIMARY_SLOT))
                .expect("row selection has a production wire representation");
            wire.send(&Frame::new(ChannelId::new(97), Kind::Event, payload))
                .expect("row selection returns to Node");
            let selected = receive_rerender(&mut wire, story);
            tree.apply(&selected, &mut surface)
                .expect("selected row acknowledgement renders in GTK");
            let cpu = view
                .columns()
                .iter::<gtk::ColumnViewColumn>()
                .filter_map(Result::ok)
                .find(|column| column.id().as_deref() == Some("cpu"))
                .expect("hard responsive specimen has an optional sortable column");
            view.sort_by_column(Some(&cpu), gtk::SortType::Descending);
            settle_toolkit();
            let _ = surface.reports().drain();
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, 600);
            root.allocate(600, 800, -1, None);
            settle_toolkit();
            assert!(
                (318..=322).contains(&table.height()),
                "narrow authored DataTable allocated {}px instead of 320px",
                table.height(),
            );
            let narrow_columns = view
                .columns()
                .iter::<gtk::ColumnViewColumn>()
                .filter_map(Result::ok)
                .filter(|column| column.is_visible())
                .filter_map(|column| column.id().map(|id| id.to_string()))
                .collect::<Vec<_>>();
            assert_eq!(
                narrow_columns,
                ["id", "name", "state", "__responsive_details:0:2,3,4"],
                "narrow DataTable did not retain identity/content and disclose optional fields",
            );
            let details = descendants::<gtk::MenuButton>(&root)
                .into_iter()
                .find(|button| button.label().as_deref() == Some("View 3 fields"))
                .expect("narrow DataTable rows expose keyboard-reachable details");
            assert!(details.is_focusable());
            assert!(details.tooltip_text().is_some_and(|text| {
                text.contains("3 hidden fields") && text.contains("Owner:") && text.contains("CPU:")
            }));
            assert!(model.is_selected(0), "narrow responsive columns lost row selection");
            let reset = surface
                .reports()
                .drain()
                .into_iter()
                .find_map(|event| match event {
                    hl_gui::Event::Sort { sort, .. } => Some(sort),
                    _ => None,
                })
                .expect("hiding the active optional sort reports its deterministic reset");
            assert_eq!(reset.column, "id");
            assert!(!reset.descending);
            capture_story(&realized_window, "DataTable narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, 1_200);
            root.allocate(1_200, 800, -1, None);
            settle_toolkit();
            assert!(
                (318..=322).contains(&table.height()),
                "wide authored DataTable allocated {}px instead of 320px",
                table.height(),
            );
            let wide_columns = view
                .columns()
                .iter::<gtk::ColumnViewColumn>()
                .filter_map(Result::ok)
                .filter(|column| column.is_visible())
                .filter_map(|column| column.id().map(|id| id.to_string()))
                .collect::<Vec<_>>();
            assert_eq!(wide_columns, ["id", "name", "owner", "cpu", "memory", "state"]);
            assert!(model.is_selected(0), "wide responsive columns lost row selection");
            capture_story(&realized_window, "DataTable");
        }
        assert!(readable_heading(&root), "{story} has no readable GTK heading");
        if story == "Bounded streaming log" {
            let buffer = find::<gtk::TextView>(&root, |_| true).buffer();
            assert_eq!(buffer.char_count(), LOG_VIEW_CHARACTER_LIMIT);
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            assert!(!text.starts_with("old history"), "oldest history was not evicted");
            assert!(
                text.contains("completed operation"),
                "newest log batch was not retained"
            );
        }
        if story == "Virtual event timeline" {
            let view = find::<gtk::ColumnView>(&root, |_| true);
            assert_eq!(view.columns().n_items(), 3, "timeline schema was not rendered");
            assert!(view.model().is_some(), "timeline has no virtualized selection model");
            assert_ne!(
                view.accessible_role(),
                gtk::AccessibleRole::Generic,
                "timeline needs a native readable collection role"
            );
        }
        if story == "Bounded key/value inspector" {
            let view = find::<gtk::ColumnView>(&root, |_| true);
            assert_eq!(view.columns().n_items(), 2, "property/value schema was not rendered");
            assert!(view.model().is_some(), "inspector has no virtualized selection model");
            assert_ne!(view.accessible_role(), gtk::AccessibleRole::Generic);
        }
        if story == "Card" {
            let cards = descendants::<gtk::Frame>(&root)
                .into_iter()
                .filter(|frame| frame.has_css_class("hl-card"))
                .collect::<Vec<_>>();
            assert_eq!(cards.len(), 5, "Card workbench must render five bounded live specimens");
            assert!(
                cards
                    .iter()
                    .all(|card| card.accessible_role() != gtk::AccessibleRole::Generic)
            );
            for card in &cards {
                let header = card
                    .label_widget()
                    .expect("CardHeader occupies the native frame header slot");
                assert!(
                    descendants::<gtk::Label>(&header)
                        .iter()
                        .any(|label| !label.text().is_empty()),
                    "Card header has no visible subject"
                );
                let body = card.child().expect("Card owns a body");
                let actions = descendants::<gtk::Box>(&body)
                    .into_iter()
                    .find(|candidate| candidate.halign() == gtk::Align::End)
                    .expect("CardActions remains after content");
                let content = body.first_child().expect("Card body starts with content");
                assert!(
                    content.has_css_class("hl-cardcontent"),
                    "CardContent keeps its semantic style identity"
                );
                assert!(
                    content.vexpands(),
                    "CardContent must absorb spare height ahead of CardActions"
                );
                assert!(
                    content.allocation().y() <= actions.allocation().y(),
                    "Card actions appeared before its content"
                );
                assert!(
                    descendants::<gtk::Button>(actions.upcast_ref())
                        .iter()
                        .all(|button| (28..=36).contains(&button.height())),
                    "Card actions are not compact controls"
                );
            }
            let long = find::<gtk::Label>(&root, |label| {
                label
                    .text()
                    .starts_with("Long identifiers and operational explanations")
            });
            let compact = cards
                .iter()
                .find(|card| {
                    descendants::<gtk::Label>(card.upcast_ref())
                        .iter()
                        .any(|label| label.text() == "Compact card")
                })
                .expect("Card sizing specimen crossed the real extension socket");
            assert!(!compact.hexpands(), "32ch Card must not inherit native fill authority");
            assert!(
                compact.width() < cards[0].width(),
                "32ch Card outer frame did not remain narrower than a fill Card"
            );
            assert!(long.wraps(), "long Card copy does not wrap");
            assert_contained(&root, "Card wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "Card narrow");
            assert!(
                !compact.hexpands(),
                "32ch Card gained fill authority after a narrow resize"
            );
            assert!(long.width() <= 552, "long Card copy escaped 16px narrow insets");
            capture_story(&realized_window, "Card narrow");
            let (status, stderr) = child.stop();
            assert!(stderr.is_empty(), "{story} wrote warnings/errors: {stderr}");
            assert!(
                !status.success(),
                "the long-running entrypoint should only end when killed"
            );
            std::fs::remove_file(socket).expect("test socket is removed");
            return;
        }
        if story == "InlineMessage" {
            for (tone, label, icon) in [
                ("neutral", "No changes to apply.", "dialog-information-symbolic"),
                ("accent", "A newer image is available.", "emblem-important-symbolic"),
                ("positive", "Workspace settings saved.", "object-select-symbolic"),
                ("warning", "Two containers will restart.", "dialog-warning-symbolic"),
                ("danger", "Network inventory is unavailable.", "dialog-error-symbolic"),
            ] {
                let message = find::<gtk::Box>(&root, |candidate| {
                    candidate.has_css_class("hl-inlinemessage")
                        && candidate.has_css_class(&format!("tone-{tone}"))
                        && descendants::<gtk::Label>(candidate.upcast_ref())
                            .iter()
                            .any(|caption| caption.text() == label)
                });
                assert_eq!(message.accessible_role(), gtk::AccessibleRole::Alert);
                assert!(
                    message.height() >= 28,
                    "{tone} message is only {}px tall",
                    message.height()
                );
                let emblem = find::<gtk::Image>(message.upcast_ref(), |_| true);
                assert_eq!(emblem.icon_name().as_deref(), Some(icon));
                assert!(emblem.is_visible(), "{tone} cue is hidden");
                assert_eq!(emblem.accessible_role(), gtk::AccessibleRole::Presentation);
                assert!(!emblem.can_focus(), "decorative {tone} icon entered keyboard order");
                if tone == "danger" {
                    let caption = find::<gtk::Label>(message.upcast_ref(), |caption| caption.text() == label);
                    let ink = caption.color();
                    assert_eq!(
                        [
                            (ink.red() * 255.0).round() as u8,
                            (ink.green() * 255.0).round() as u8,
                            (ink.blue() * 255.0).round() as u8,
                        ],
                        [0xff, 0x90, 0x90],
                        "danger Alert resolves the shared AAA error-text token"
                    );
                }
            }
            let override_message = find::<gtk::Box>(&root, |candidate| {
                candidate.has_css_class("hl-inlinemessage")
                    && descendants::<gtk::Label>(candidate.upcast_ref())
                        .iter()
                        .any(|caption| caption.text() == "Connected through the workspace network.")
            });
            assert_eq!(
                find::<gtk::Image>(override_message.upcast_ref(), |_| true)
                    .icon_name()
                    .as_deref(),
                Some("network-workgroup-symbolic"),
                "explicit icon did not override the tone default"
            );
            let long = find::<gtk::Box>(&root, |candidate| {
                candidate.has_css_class("hl-inlinemessage")
                    && descendants::<gtk::Label>(candidate.upcast_ref()).iter().any(|caption| {
                        caption.text()
                            == "Network inventory is unavailable. Check that the workspace is running, then retry."
                    })
            });
            assert!(long.width() <= root.width(), "long message overflowed its page");
            assert!(
                descendants::<gtk::Label>(long.upcast_ref())
                    .iter()
                    .any(gtk::Label::wraps),
                "long message caption does not wrap"
            );

            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, 300);
            allocate(&root, 300, 1_600);
            assert_contained(&root, story);
            let (status, stderr) = child.stop();
            assert!(stderr.is_empty(), "{story} wrote warnings/errors: {stderr}");
            assert!(
                !status.success(),
                "the long-running entrypoint should only end when killed"
            );
            std::fs::remove_file(socket).expect("test socket is removed");
            return;
        }

        let event = emit_representative(story, &root, &surface, &tree);
        if story == "Checkbox" {
            let hl_gui::Event::Toggle { value, .. } = &event else {
                panic!("mixed Checkbox did not emit its typed Toggle interaction: {event:?}")
            };
            assert_eq!(
                value,
                &hl_gui::PropValue::Flag(true),
                "activating mixed Checkbox emits the resolved checked value"
            );
        }
        let payload = codec::interaction(&event, Some(PRIMARY_SLOT))
            .unwrap_or_else(|| panic!("{story} interaction has no production wire encoding"));
        wire.send(&Frame::new(ChannelId::new(3), Kind::Event, payload))
            .expect("interaction returns to Node");
        let rerender = receive_rerender(&mut wire, story);
        assert!(
            !rerender.patches.is_empty(),
            "{story} interaction produced an empty rerender"
        );
        assert!(
            rerender.patches.len() <= PATCH_LIMIT,
            "{story} rerender emitted {} patches",
            rerender.patches.len()
        );
        tree.apply(&rerender, &mut surface)
            .unwrap_or_else(|error| panic!("{story} rerender failed in GTK: {error:?}"));
        if story == "Expander" {
            let disclosure = find::<gtk::Expander>(&root, |expander| {
                expander.label().as_deref() == Some("Runtime diagnostics")
            });
            assert!(disclosure.is_expanded(), "controlled state survives its rerender");
            assert!(disclosure.has_focus(), "controlled rerender preserves summary focus");
        }
        if story == "RecoveryState" {
            find::<gtk::Label>(&root, |label| label.text() == "Attempt 2");
            let mut disclosures = descendants::<gtk::Expander>(&root)
                .into_iter()
                .filter(|expander| expander.label().as_deref() == Some("Technical details"))
                .collect::<Vec<_>>();
            disclosures.sort_by(|left, right| {
                left.compute_bounds(&root)
                    .expect("disclosure belongs to RecoveryState document")
                    .y()
                    .total_cmp(
                        &right
                            .compute_bounds(&root)
                            .expect("disclosure belongs to RecoveryState document")
                            .y(),
                    )
            });
            assert_eq!(disclosures.len(), 2, "RecoveryState page owns exactly two variants");
            let disclosure = disclosures[0].clone();
            let partial = disclosures[1].clone();
            assert!(
                disclosure.grab_focus(),
                "RecoveryState disclosure accepts keyboard focus"
            );
            disclosure.emit_by_name::<()>("activate", &[]);
            settle_toolkit();
            assert!(disclosure.is_expanded(), "RecoveryState reveals its bounded diagnostic");
            find::<gtk::Label>(&root, |label| label.text() == "socket closed during attempt 2");
            capture_story(&realized_window, "RecoveryState expanded");
            disclosure.emit_by_name::<()>("activate", &[]);
            partial.emit_by_name::<()>("activate", &[]);
            settle_toolkit();
            assert!(!disclosure.is_expanded(), "the complete failure diagnostic closes");
            assert!(
                partial.is_expanded(),
                "the partial-result diagnostic opens independently"
            );
            find::<gtk::Label>(&root, |label| {
                label.text() == "worker: process endpoint did not respond"
            });
            capture_story(&realized_window, "RecoveryState partial expanded");
        }
        if story == "Search" {
            let search = find::<gtk::SearchEntry>(&root, |entry| {
                entry.tooltip_text().as_deref() == Some("Find extensions")
            });
            assert_eq!(search.text(), "runtime");
            find::<gtk::Label>(&root, |label| label.text() == "Filtering by “runtime”.");
            capture_story(&realized_window, "Search changed wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "Search changed narrow");
            assert!(search.width() <= 552);
            capture_story(&realized_window, "Search changed narrow");
        }
        if story == "TextArea" {
            let editor = find::<gtk::ScrolledWindow>(&root, |window| {
                window.tooltip_text().as_deref() == Some("Task manifest")
            });
            let view = editor
                .child()
                .and_then(|child| child.downcast::<gtk::TextView>().ok())
                .expect("controlled TextArea retains its native editor");
            let buffer = view.buffer();
            assert_eq!(
                buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
                "name: compile\ncommand: cargo test"
            );
            find::<gtk::Label>(&root, |label| label.text() == "2 lines · 33 characters");
            capture_story(&realized_window, "TextArea changed wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "TextArea changed narrow");
            capture_story(&realized_window, "TextArea changed narrow");
        }
        if story == "NumberEntry" {
            let counter =
                find::<gtk::SpinButton>(&root, |spin| spin.tooltip_text().as_deref() == Some("Build workers"));
            assert_eq!(counter.value(), 6.0);
            find::<gtk::Label>(&root, |label| label.text() == "6 concurrent workers");
            capture_story(&realized_window, "NumberEntry changed wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "NumberEntry changed narrow");
            capture_story(&realized_window, "NumberEntry changed narrow");
        }
        if story == "PasswordEntry" {
            let field =
                find::<gtk::PasswordEntry>(&root, |entry| entry.tooltip_text().as_deref() == Some("Registry token"));
            assert_eq!(field.text(), "rotated-token");
            find::<gtk::Label>(&root, |label| label.text() == "13 characters · concealed");
            assert!(
                descendants::<gtk::Label>(&root)
                    .iter()
                    .all(|label| label.text() != "rotated-token"),
                "secret leaked into visible feedback"
            );
            capture_story(&realized_window, "PasswordEntry changed wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "PasswordEntry changed narrow");
            capture_story(&realized_window, "PasswordEntry changed narrow");
        }
        if story == "Autocomplete" {
            let choice = find::<gtk::DropDown>(&root, |drop| drop.tooltip_text().as_deref() == Some("Runtime"));
            assert_eq!(choice.selected(), 1);
            find::<gtk::Label>(&root, |label| label.text() == "Selected Python 3.13.");
            capture_story(&realized_window, "Autocomplete selected wide");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "Autocomplete selected narrow");
            capture_story(&realized_window, "Autocomplete selected narrow");
        }
        if let Some(before) = toggle_before {
            settle_toolkit();
            let toggle =
                find::<gtk::ToggleButton>(&root, |button| button.tooltip_text().as_deref() == Some("Pin this tab"));
            assert_ne!(
                toggle.is_active(),
                before,
                "documented ToggleButton did not visibly acknowledge its checked-state change"
            );
            assert_eq!(toggle.accessible_role(), gtk::AccessibleRole::ToggleButton);
            assert_toggle_receipt(&root, "No change yet.", 36);
            capture_story(&realized_window, "ToggleButton unchecked");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            settle_window_width(&realized_window, 600);
            assert_contained(&root, "ToggleButton post-event narrow");
            assert_toggle_receipt(&root, "No change yet.", 56);
            capture_story(&realized_window, "ToggleButton unchecked narrow");
        }
        if story == "Checkbox" {
            settle_toolkit();
            let checkbox =
                find::<gtk::CheckButton>(&root, |button| button.label().as_deref() == Some("Select all · 3 of 3"));
            assert!(checkbox.is_active(), "mixed parent did not resolve to checked");
            assert!(!checkbox.is_inconsistent(), "resolved parent remained indeterminate");
            assert!(checkbox.grab_focus(), "controlled Checkbox restores native focus");
            capture_story(&realized_window, "Checkbox focused checked");
        }
        if story == "Radio" {
            settle_toolkit();
            let bash = find::<gtk::CheckButton>(&root, |button| button.label().as_deref() == Some("Bash"));
            assert!(bash.is_active(), "controlled Radio did not retain native selection");
            assert!(bash.grab_focus(), "controlled Radio restores native focus");
            assert!(bash.has_focus(), "controlled Radio exposes focus-visible state");
            assert_eq!(bash.accessible_role(), gtk::AccessibleRole::Radio);
            assert_eq!(bash.height(), 18, "Radio keeps its compact 16px indicator row");
            assert!(
                bash.width() >= 900,
                "wide Radio fixture did not exercise an expanding row: {}px",
                bash.width()
            );
            capture_story(&realized_window, "Radio focused bash");
        }
        if story == "RadioGroup" {
            settle_toolkit();
            let nightly = find::<gtk::CheckButton>(&root, |button| button.label().as_deref() == Some("Nightly"));
            assert!(
                nightly.is_active(),
                "controlled RadioGroup did not retain native selection"
            );
            assert!(
                nightly.grab_focus(),
                "controlled RadioGroup restores its roving focus target"
            );
            assert!(nightly.has_focus(), "controlled RadioGroup exposes focus-visible state");
            capture_story(&realized_window, "RadioGroup focused nightly");
        }
        if story == "FormControl" {
            settle_toolkit();
            let entry = find::<gtk::Entry>(&root, |entry| entry.tooltip_text().as_deref() == Some("Extension name"));
            assert!(entry.grab_focus(), "controlled FormControl restores child focus");
            capture_story(&realized_window, "FormControl focused");
        }
        if story == "Slider" {
            settle_toolkit();
            let slider = find::<gtk::Scale>(&root, |scale| {
                scale.tooltip_text().as_deref() == Some("Build cache allocation")
            });
            assert_eq!(slider.value(), 45.0, "controlled Slider retains the emitted value");
            assert!(slider.grab_focus(), "controlled Slider restores native focus");
            capture_story(&realized_window, "Slider focused");
            let disclosure = find::<gtk::Expander>(&root, |expander| expander.label().as_deref() == Some("Show code"));
            disclosure.set_expanded(true);
            settle_toolkit();
            assert!(disclosure.is_expanded(), "Slider code disclosure opens natively");
            assert!(
                find::<gtk::Label>(&root, |label| label.text().contains("boundedStep(report.value)")).is_visible(),
                "expanded Slider disclosure exposes the exact report handler"
            );
            capture_story(&realized_window, "Slider code expanded");
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            allocate(&root, 600, 800);
            settle_toolkit();
            assert_contained(&root, "Slider expanded code");
            capture_story(&realized_window, "Slider code expanded narrow");
        }
        if story == "Extension acquisition" {
            let card = find::<gtk::Frame>(&root, |frame| frame.has_css_class("hl-card"));
            assert!(
                card.height() <= 180,
                "a standalone acquisition card consumed {}px instead of staying content-sized",
                card.height()
            );
            let action = find::<gtk::Button>(&card.clone().upcast(), |button| {
                button_caption(button).as_deref() == Some("Cancel download")
            });
            assert!(
                action.allocation().y() <= 100,
                "the acquisition action drifted {}px away from its status",
                action.allocation().y()
            );
            assert!(
                find::<gtk::Label>(&root, |label| label.text() == "Cancel download invoked for checking.").is_visible(),
                "cancellation did not produce visible, state-specific acknowledgement"
            );
        }
        if story == "Validated settings form" {
            assert!(
                descendants::<gtk::ToggleButton>(&root)
                    .iter()
                    .all(|button| button.label().as_deref() != Some("backend")),
                "the controlled form did not acknowledge removal of the activated tag"
            );
        }
        if story == "Navigation and transient UI" {
            assert!(
                !find::<gtk::Expander>(&root, |_| true).is_expanded(),
                "the controlled disclosure did not acknowledge the native collapse"
            );
        }
        if story == "DataTable" {
            surface
                .resize(hl_gui::SourceId::new(100), hl_gui::Version::new(2), 100_000)
                .expect("accepted edit advances the GTK source version");
            settle_toolkit();
            let requests = surface.requests(128);
            assert!(!requests.is_empty(), "new source version requests fresh windows");
            let mut accepted = false;
            for (offset, request) in requests.into_iter().enumerate() {
                let channel = ChannelId::new(5 + offset as u32);
                wire.send(&Frame::new(
                    channel,
                    Kind::Event,
                    serde_json::to_vec(&request).expect("accepted-edit row request encodes"),
                ))
                .expect("accepted-edit row request reaches Storybook");
                let edit_deadline = Instant::now() + SOCKET_DEADLINE;
                let answer = loop {
                    let carried =
                        receive_until(&mut wire, edit_deadline).expect("Storybook answers the accepted-edit window");
                    if carried.kind == Kind::Response && carried.channel == channel {
                        break carried;
                    }
                    if carried.kind == Kind::Credit {
                        continue;
                    }
                    let follow_up = codec::read_request(&carried).expect("concurrent Storybook call decodes");
                    apply_concurrent(&follow_up, &mut tree, &mut surface);
                    wire.send(&codec::reply(&Reply::Done).expect("follow-up reply encodes"))
                        .expect("follow-up reply sends");
                };
                let window: hl_gui::RowWindow =
                    serde_json::from_slice(&answer.payload).expect("accepted-edit row window decodes");
                accepted |= window.rows.iter().any(|row| {
                    row.cells
                        .iter()
                        .any(|cell| matches!(cell, hl_gui::Cell::Text(value) if value == "needle"))
                });
                surface.rows(&window).expect("GTK accepts the newer row window");
            }
            assert!(accepted, "accepted producer windows omitted the committed edit");
            settle_toolkit();
            assert_eq!(
                find::<gtk::Entry>(&root, |entry| entry.text() == "needle").text(),
                "needle",
                "only an accepted newer window replaces the controlled cell"
            );
            let hl_gui::Event::Edit { node, id, mut edit } = event.clone() else {
                unreachable!("the DataTable representative event is an edit")
            };
            edit.version = hl_gui::Version::new(1);
            edit.value = "stale overwrite".to_owned();
            let stale = hl_gui::Event::Edit { node, id, edit };
            let payload = codec::interaction(&stale, Some(PRIMARY_SLOT)).expect("stale edit has a wire representation");
            wire.send(&Frame::new(ChannelId::new(98), Kind::Event, payload))
                .expect("stale native edit returns to Node");
            let rejected = receive_rejected_edit(&mut wire);
            tree.apply(&rejected, &mut surface)
                .expect("stale-edit rejection renders in GTK");
            settle_toolkit();
            assert_eq!(
                find::<gtk::Entry>(&root, |entry| entry.text() == "needle").text(),
                "needle",
                "a stale edit cannot replace authoritative row text"
            );
            assert!(
                find::<gtk::Label>(&root, |label| label.text().contains("edit refused: stale version")).is_visible(),
                "the stale rejection is visible in the bounded operation history"
            );
            let view = find::<gtk::ColumnView>(&root, |_| true);
            let column = view
                .columns()
                .item(0)
                .and_downcast::<gtk::ColumnViewColumn>()
                .expect("sortable ID column");
            view.sort_by_column(Some(&column), gtk::SortType::Descending);
            settle_toolkit();
            let event = surface
                .reports()
                .drain()
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Sort { .. }))
                .expect("native header publishes a sort proposal");
            let hl_gui::Event::Sort { sort, .. } = &event else {
                unreachable!()
            };
            assert_eq!(sort.source, hl_gui::SourceId::new(100));
            assert_eq!(sort.version, hl_gui::Version::new(2));
            assert_eq!(sort.column, "id");
            assert!(sort.descending);
            let payload = codec::interaction(&event, Some(PRIMARY_SLOT)).expect("sort has a wire representation");
            wire.send(&Frame::new(ChannelId::new(99), Kind::Event, payload))
                .expect("native sort returns to Node");
            let sorted = receive_rerender(&mut wire, story);
            assert!(
                !sorted.patches.is_empty(),
                "accepted native sort is observable in the story"
            );
            let view = find::<gtk::ColumnView>(&root, |_| true);
            assert!(view.grab_focus(), "ready DataTable accepts keyboard focus");
            let model = view.model().expect("rerendered DataTable keeps its selection model");
            assert!(model.select_item(0, true), "selected evidence survives the rerender");
            settle_toolkit();
            capture_story(&realized_window, "DataTable ready selected");
        }
        root.measure(gtk::Orientation::Horizontal, -1);
        root.measure(gtk::Orientation::Vertical, 300);
        allocate(&root, 300, 1_600);
        assert_contained(&root, story);
        assert!(
            readable_heading(&root),
            "{story} lost its readable heading after interaction"
        );
        if story == "Bounded streaming log" {
            assert_eq!(
                find::<gtk::TextView>(&root, |_| true).buffer().char_count(),
                LOG_VIEW_CHARACTER_LIMIT,
                "appending a batch exceeded fixed retention"
            );
        }

        let (status, stderr) = child.stop();
        assert!(stderr.is_empty(), "{story} wrote warnings/errors: {stderr}");
        assert!(
            !status.success(),
            "the long-running entrypoint should only end when killed"
        );
        std::fs::remove_file(socket).expect("test socket is removed");
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

    fn receive_before(wire: &mut Wire<UnixStream>, duration: Duration) -> Result<Frame, hl_extension::Transit> {
        receive_until(wire, Instant::now() + duration)
    }

    fn receive_until(wire: &mut Wire<UnixStream>, deadline: Instant) -> Result<Frame, hl_extension::Transit> {
        loop {
            match wire.receive_step() {
                Err(hl_extension::Transit::Pending) if Instant::now() < deadline => {}
                Err(hl_extension::Transit::Pending) => return Err(hl_extension::Transit::Pending),
                result => return result,
            }
        }
    }

    #[test]
    fn a_peer_trickling_a_partial_frame_cannot_extend_the_wall_clock_deadline() {
        use std::io::Write as _;

        let (reader, mut writer) = UnixStream::pair().expect("socket pair");
        reader
            .set_read_timeout(Some(Duration::from_millis(10)))
            .expect("short read tick");
        let sending = std::thread::spawn(move || {
            let bytes = Frame::control(Kind::Ping, b"still incomplete".to_vec())
                .encode()
                .expect("frame encodes");
            for byte in bytes.into_iter().take(8) {
                if writer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let mut wire = Wire::new(reader);
        let started = Instant::now();

        assert_eq!(
            receive_before(&mut wire, Duration::from_millis(75)),
            Err(hl_extension::Transit::Pending)
        );
        assert!(
            started.elapsed() < Duration::from_millis(140),
            "a peer's byte trickle extended the receive deadline"
        );
        drop(wire);
        sending.join().expect("trickle writer exits");
    }

    fn capture_story(window: &gtk::Window, story: &str) {
        capture_widget(window, window.upcast_ref::<gtk::Widget>(), story);
    }

    fn capture_widget(window: &gtk::Window, widget: &gtk::Widget, story: &str) {
        let Some(directory) = std::env::var_os("STORYBOOK_SHOT") else {
            return;
        };
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).expect("Storybook screenshot directory is created");
        let name = story
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let paintable = gtk::WidgetPaintable::new(Some(widget));
        let node = (0..20)
            .find_map(|_| {
                window.queue_draw();
                settle_toolkit();
                let snapshot = gtk::Snapshot::new();
                paintable.snapshot(
                    snapshot.upcast_ref::<gtk::gdk::Snapshot>(),
                    f64::from(widget.width()),
                    f64::from(widget.height()),
                );
                let node = snapshot.to_node();
                if node.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                node
            })
            .expect("Storybook window produces a render node");
        let renderer = window.renderer().expect("Storybook window has a renderer");
        renderer
            .render_texture(&node, None)
            .save_to_png(directory.join(format!("{name}.png")))
            .expect("Storybook screenshot is written");
    }

    fn receive_rerender(wire: &mut Wire<std::os::unix::net::UnixStream>, story: &str) -> hl_gui::Frame {
        let deadline = Instant::now() + SOCKET_DEADLINE;
        for _ in 0..8 {
            let carried = receive_until(wire, deadline).expect("Node answers the GTK interaction");
            if carried.kind == Kind::Credit {
                continue;
            }
            let request = codec::read_request(&carried).expect("post-interaction request decodes");
            let frame = match request {
                Request::InterfaceRenderAt { slot, frame } => {
                    assert_eq!(slot, PRIMARY_SLOT);
                    Some(frame)
                }
                Request::SourceResizeAt { .. } => None,
                other => panic!("unexpected post-interaction call from {story}: {other:?}"),
            };
            wire.send(&codec::reply(&Reply::Done).expect("done encodes"))
                .expect("post-interaction reply sends");
            if let Some(frame) = frame {
                return frame;
            }
        }
        panic!("{story} did not rerender after its GTK interaction")
    }

    fn receive_rejected_edit(wire: &mut Wire<std::os::unix::net::UnixStream>) -> hl_gui::Frame {
        let deadline = Instant::now() + SOCKET_DEADLINE;
        for _ in 0..8 {
            let carried = receive_until(wire, deadline).expect("Node answers the stale native edit");
            if carried.kind == Kind::Credit {
                continue;
            }
            match codec::read_request(&carried).expect("stale-edit response decodes") {
                Request::InterfaceRenderAt { slot, frame } => {
                    assert_eq!(slot, PRIMARY_SLOT);
                    wire.send(&codec::reply(&Reply::Done).expect("rejection acknowledgement encodes"))
                        .expect("rejection acknowledgement sends");
                    return frame;
                }
                Request::SourceResizeAt { mutation, .. } => {
                    panic!("rejected stale edit advanced its source: {mutation:?}")
                }
                other => panic!("unexpected stale-edit call: {other:?}"),
            }
        }
        panic!("stale edit produced no visible rejection")
    }

    fn apply_concurrent(request: &Request, tree: &mut Tree, surface: &mut Surface) {
        match request {
            Request::InterfaceRenderAt { slot, frame } => {
                assert_eq!(slot, PRIMARY_SLOT);
                tree.apply(frame, surface).expect("concurrent Storybook render applies");
            }
            Request::SourceResizeAt { slot, .. } => {
                assert_eq!(slot, PRIMARY_SLOT);
            }
            other => panic!("unexpected concurrent Storybook call: {other:?}"),
        }
    }

    fn emit_representative(story: &str, root: &gtk::Widget, surface: &Surface, tree: &Tree) -> hl_gui::Event {
        match story {
            "Autocomplete" => {
                find::<gtk::DropDown>(root, |drop| drop.tooltip_text().as_deref() == Some("Runtime")).set_selected(1);
            }
            "Button" => {
                find::<gtk::Button>(root, |button| button_caption(button).as_deref() == Some("Run task"))
                    .emit_clicked();
            }
            "IconButton" => {
                find::<gtk::Button>(root, |button| button.tooltip_text().as_deref() == Some("Refresh")).emit_clicked();
            }
            "Entry" => {
                let entry = find::<gtk::Entry>(root, |entry| entry.tooltip_text().as_deref() == Some("Extension name"));
                entry.set_text("rendered-entry");
            }
            "Search" => {
                let search =
                    find::<gtk::SearchEntry>(root, |entry| entry.tooltip_text().as_deref() == Some("Find extensions"));
                search.set_text("runtime");
            }
            "NumberEntry" => {
                find::<gtk::SpinButton>(root, |spin| spin.tooltip_text().as_deref() == Some("Build workers"))
                    .set_value(6.0);
            }
            "PasswordEntry" => {
                find::<gtk::PasswordEntry>(root, |entry| entry.tooltip_text().as_deref() == Some("Registry token"))
                    .set_text("rotated-token");
            }
            "TextArea" => {
                let editor = find::<gtk::ScrolledWindow>(root, |window| {
                    window.tooltip_text().as_deref() == Some("Task manifest")
                });
                let view = editor
                    .child()
                    .and_then(|child| child.downcast::<gtk::TextView>().ok())
                    .expect("TextArea owns its native editor");
                view.buffer().set_text("name: compile\ncommand: cargo test");
            }
            "FormControl" => {
                let entry = find::<gtk::Entry>(root, |entry| entry.tooltip_text().as_deref() == Some("Extension name"));
                entry.set_text("rendered-form-control");
            }
            "Slider" => {
                let slider = find::<gtk::Scale>(root, |scale| {
                    scale.tooltip_text().as_deref() == Some("Build cache allocation")
                });
                slider.set_value(45.0);
            }
            "Select" => {
                let choice =
                    find::<gtk::ToggleButton>(root, |button| button.tooltip_text().as_deref() == Some("Default shell"));
                assert!(choice.grab_focus(), "Select accepts keyboard focus");
            }
            "Heading" => {
                let document = descendants::<gtk::ScrolledWindow>(root)
                    .into_iter()
                    .max_by(|left, right| left.vadjustment().upper().total_cmp(&right.vadjustment().upper()))
                    .expect("Heading document owns a scrolling viewport");
                let adjustment = document.vadjustment();
                adjustment.set_value(adjustment.upper() - adjustment.page_size());
                settle_toolkit();
                let playground = find::<gtk::Expander>(root, |expander| {
                    descendants::<gtk::Label>(expander.upcast_ref())
                        .iter()
                        .any(|label| label.text() == "Playground")
                });
                playground.set_expanded(true);
                assert!(playground.grab_focus(), "Heading Playground accepts focus");
                settle_toolkit();
                let choice = find::<gtk::ToggleButton>(root, |button| {
                    button.accessible_role() == gtk::AccessibleRole::ComboBox
                        && descendants::<gtk::Button>(button.upcast_ref())
                            .iter()
                            .any(|option| button_caption(option).as_deref() == Some("display"))
                });
                assert!(
                    choice.width() <= 196,
                    "Heading scale selector expanded to {}px",
                    choice.width()
                );
                choice.emit_clicked();
                settle_toolkit();
                let option = find::<gtk::Button>(choice.upcast_ref(), |button| {
                    button_caption(button).as_deref() == Some("display")
                });
                let deadline = Instant::now() + Duration::from_millis(500);
                while !option.is_mapped() && Instant::now() < deadline {
                    settle_toolkit();
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(
                    option.is_mapped(),
                    "Heading option must finish opening before interaction"
                );
                option.emit_clicked();
                settle_toolkit();
            }
            "Expander" => {
                let disclosure = find::<gtk::Expander>(root, |expander| {
                    expander.label().as_deref() == Some("Runtime diagnostics")
                });
                assert!(disclosure.grab_focus(), "Expander summary accepts keyboard focus");
                disclosure.emit_by_name::<()>("activate", &[]);
            }
            "RecoveryState" => {
                find::<gtk::Button>(root, |button| button_caption(button).as_deref() == Some("Try again"))
                    .emit_clicked();
            }
            "Switch" => {
                labelled_switch(root, "Restore panes on launch").set_active(false);
            }
            "ToggleButton" => {
                find::<gtk::ToggleButton>(root, |button| button.tooltip_text().as_deref() == Some("Pin this tab"))
                    .emit_clicked();
            }
            "Checkbox" => {
                let checkbox =
                    find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Select all · 1 of 3"));
                assert!(checkbox.is_inconsistent(), "parent Checkbox exposes native mixed state");
                assert!(checkbox.grab_focus(), "Checkbox accepts native keyboard focus");
                assert!(checkbox.has_focus(), "Checkbox exposes its focus-visible state");
                checkbox.activate();
            }
            "Radio" => {
                let zsh = find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Z shell"));
                let bash = find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Bash"));
                let fish = find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Fish"));
                assert!(zsh.is_active());
                assert!(zsh.grab_focus(), "Radio group accepts native keyboard focus");
                assert!(zsh.has_focus(), "focused Radio exposes focus-visible state");
                assert!(
                    root.child_focus(gtk::DirectionType::Down),
                    "Radio group accepts directional key movement"
                );
                assert!(bash.has_focus(), "directional movement focuses the next Radio");
                bash.activate();
                assert!(bash.is_active(), "native group movement selects the next Radio");
                assert!(!zsh.is_active(), "native group preserves exactly one selection");
                assert!(!fish.is_active(), "native group leaves every other Radio unselected");
            }
            "RadioGroup" => {
                let stable = find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Stable"));
                let preview = find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Preview"));
                let nightly = find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Nightly"));
                assert!(stable.is_active());
                assert!(!preview.is_sensitive(), "disabled Radio child remains unavailable");
                assert!(stable.grab_focus(), "RadioGroup has one selected tab stop");
                assert!(
                    root.child_focus(gtk::DirectionType::Down),
                    "RadioGroup accepts directional movement"
                );
                assert!(!preview.has_focus(), "directional movement skips the disabled Radio");
                assert!(
                    nightly.has_focus(),
                    "directional movement reaches the next enabled Radio"
                );
                nightly.activate();
                assert!(nightly.is_active(), "directional target becomes selected");
                assert!(!stable.is_active(), "RadioGroup retains exactly one native selection");
            }
            "DataTable" => {
                let entry = find::<gtk::Entry>(root, |entry| entry.text().starts_with("record-"));
                let authoritative = entry.text();
                let row = authoritative.strip_prefix("record-").expect("visible row identity");
                let accessible_name = format!("Workspace record, row {row}");
                assert_eq!(
                    entry.tooltip_text().as_deref(),
                    Some(accessible_name.as_str()),
                    "the exact name shared with the accessible Label must identify column and row"
                );
                entry.set_text("needle");
                entry.emit_by_name::<()>("activate", &[]);
                assert_eq!(
                    entry.text(),
                    authoritative,
                    "an unacknowledged draft must not replace authoritative row text"
                );
            }
            "Keyboard and semantic actions" => {
                let entry = find::<gtk::Entry>(root, |entry| entry.placeholder_text().as_deref() == Some("storybook"));
                entry.set_text("storybook");
            }
            "Drag and keyboard reorder" => {
                find::<gtk::Button>(root, |button| button_caption(button).as_deref() == Some("↓ Build")).emit_clicked();
            }
            "Workspace layout control" => {
                find::<gtk::Button>(root, |button| {
                    button_caption(button).as_deref() == Some("Open pane chooser")
                })
                .emit_clicked();
            }
            "Extension acquisition" => {
                let cancel = find::<gtk::Button>(root, |button| {
                    button_caption(button).as_deref() == Some("Cancel download")
                });
                assert_eq!(
                    cancel.accessible_role(),
                    gtk::AccessibleRole::Button,
                    "pending acquisition cancellation must remain a native accessible button"
                );
                cancel.emit_clicked();
            }
            "Validated settings form" => {
                find::<gtk::ToggleButton>(root, |button| button.label().as_deref() == Some("backend")).emit_clicked();
            }
            "Navigation and transient UI" => {
                find::<gtk::Expander>(root, |_| true).set_expanded(false);
            }
            "Bounded streaming log" => {
                find::<gtk::Button>(root, |button| button_caption(button).as_deref() == Some("Append batch"))
                    .emit_clicked();
            }
            "Virtual event timeline" => {
                find::<gtk::Button>(root, |button| {
                    button_caption(button).as_deref() == Some("Acknowledge newest")
                })
                .emit_clicked();
            }
            "Bounded key/value inspector" => {
                find::<gtk::Button>(root, |button| {
                    button_caption(button).as_deref() == Some("Refresh metadata")
                })
                .emit_clicked();
            }
            _ => unreachable!(),
        }
        settle_toolkit();
        let reports = surface.reports().drain();
        assert!(
            (1..=2).contains(&reports.len()),
            "{story} emitted {reports:?} instead of a bounded event"
        );
        let event = if story == "Autocomplete" {
            reports
                .into_iter()
                .find(|event| matches!(event, hl_gui::Event::Select { rows, .. } if rows == &[1]))
                .expect("Autocomplete reports the selected row")
        } else if matches!(story, "Radio" | "RadioGroup") {
            reports
                .into_iter()
                .find(|event| {
                    matches!(
                        event,
                        hl_gui::Event::Toggle {
                            value: hl_gui::PropValue::Flag(true),
                            ..
                        }
                    )
                })
                .expect("Radio reports its newly selected option")
        } else if story == "Search" {
            reports
                .into_iter()
                .find(|event| {
                    matches!(
                        event,
                        hl_gui::Event::Change {
                            value: hl_gui::PropValue::Text(value),
                            ..
                        } if value == "runtime"
                    )
                })
                .expect("Search reports its final native edit value")
        } else if story == "TextArea" {
            reports
                .into_iter()
                .find(|event| {
                    matches!(
                        event,
                        hl_gui::Event::Change {
                            value: hl_gui::PropValue::Text(value),
                            ..
                        } if value == "name: compile\ncommand: cargo test"
                    )
                })
                .expect("TextArea reports its final native edit value")
        } else if story == "PasswordEntry" {
            reports
                .into_iter()
                .find(|event| {
                    matches!(
                        event,
                        hl_gui::Event::Change {
                            value: hl_gui::PropValue::Text(value),
                            ..
                        } if value == "rotated-token"
                    )
                })
                .expect("PasswordEntry reports its final native edit value")
        } else {
            reports.into_iter().next().expect("one report")
        };
        if story == "Search" {
            let hl_gui::Event::Change { value, .. } = &event else {
                panic!("Search did not emit its typed Change interaction: {event:?}")
            };
            assert_eq!(value, &hl_gui::PropValue::text("runtime"));
        }
        if story == "NumberEntry" {
            let hl_gui::Event::Change { value, .. } = &event else {
                panic!("NumberEntry did not emit its typed Change interaction: {event:?}")
            };
            assert_eq!(value, &hl_gui::PropValue::Number(6.0));
        }
        if story == "PasswordEntry" {
            let hl_gui::Event::Change { value, .. } = &event else {
                panic!("PasswordEntry did not emit its typed Change interaction: {event:?}")
            };
            assert_eq!(value, &hl_gui::PropValue::text("rotated-token"));
        }
        if story == "TextArea" {
            let hl_gui::Event::Change { value, .. } = &event else {
                panic!("TextArea did not emit its typed Change interaction: {event:?}")
            };
            assert_eq!(value, &hl_gui::PropValue::text("name: compile\ncommand: cargo test"));
        }
        if story == "Extension acquisition" {
            let hl_gui::Event::Invoke { node, id } = &event else {
                panic!("native cancellation did not emit its typed Invoke interaction: {event:?}")
            };
            assert_eq!(
                tree.handler(*node, hl_gui::Trigger::Invoke),
                Some(id),
                "native cancellation must preserve the producer-owned handler identity"
            );
        }
        if story == "Validated settings form" {
            let hl_gui::Event::Toggle { node, id, value } = &event else {
                panic!("native ToggleButton did not emit its typed Toggle interaction: {event:?}")
            };
            assert_eq!(
                tree.handler(*node, hl_gui::Trigger::Toggle),
                Some(id),
                "native ToggleButton must preserve the producer-owned handler identity"
            );
            assert_eq!(
                value,
                &hl_gui::PropValue::Flag(false),
                "native ToggleButton must report its released state"
            );
        }
        if story == "Navigation and transient UI" {
            let hl_gui::Event::Expand { node, id, value } = &event else {
                panic!("native Expander did not emit its typed Expand interaction: {event:?}")
            };
            assert_eq!(
                tree.handler(*node, hl_gui::Trigger::Expand),
                Some(id),
                "native Expander must preserve the producer-owned Expand handler identity"
            );
            assert_eq!(
                value,
                &hl_gui::PropValue::Flag(false),
                "native Expander must report the collapsed state"
            );
        }
        if story == "Expander" {
            let hl_gui::Event::Expand { node, id, value } = &event else {
                panic!("native Expander did not emit its typed Expand interaction: {event:?}")
            };
            assert_eq!(
                tree.handler(*node, hl_gui::Trigger::Expand),
                Some(id),
                "controlled Expander preserves its producer-owned handler identity"
            );
            assert_eq!(value, &hl_gui::PropValue::Flag(true));
        }
        if story == "Keyboard and semantic actions" {
            let hl_gui::Event::Change { node, .. } = event else {
                panic!("entry did not emit Change")
            };
            let id = tree
                .handler(node, hl_gui::Trigger::Focus)
                .expect("entry declares Focus")
                .clone();
            return hl_gui::Event::Focus {
                node,
                id,
                focused: true,
            };
        }
        event
    }

    fn button_caption(button: &gtk::Button) -> Option<String> {
        if let Some(label) = button.label() {
            return Some(label.to_string());
        }
        descendants::<gtk::Label>(button.upcast_ref())
            .into_iter()
            .find(|label| label.has_css_class("hl-caption"))
            .map(|label| label.text().to_string())
    }

    fn labelled_switch(root: &gtk::Widget, caption: &str) -> gtk::Switch {
        find::<gtk::Label>(root, |label| label.text() == caption)
            .mnemonic_widget()
            .and_then(|widget| widget.downcast::<gtk::Switch>().ok())
            .expect("FormControlLabel caption names its Switch")
    }

    fn assert_recovery_state(root: &gtk::Widget, case: &str) {
        let message = find::<gtk::Box>(root, |candidate| {
            candidate.has_css_class("hl-inlinemessage")
                && descendants::<gtk::Label>(candidate.upcast_ref())
                    .iter()
                    .any(|label| label.text() == "Container inventory lost its connection. No change was assumed.")
        });
        assert_eq!(message.accessible_role(), gtk::AccessibleRole::Alert);
        let retry = find::<gtk::Button>(root, |button| button_caption(button).as_deref() == Some("Try again"));
        assert_eq!(retry.accessible_role(), gtk::AccessibleRole::Button);
        assert!(retry.is_focusable(), "{case} RecoveryState retry is keyboard reachable");
        assert!(retry.has_css_class("size-small"));
        assert_eq!(retry.height(), 28, "{case} RecoveryState retry control height");
        let disclosure = find::<gtk::Expander>(root, |expander| {
            expander.label().as_deref() == Some("Technical details")
        });
        assert_eq!(disclosure.accessible_role(), gtk::AccessibleRole::Button);
        assert!(
            disclosure.is_focusable(),
            "{case} RecoveryState disclosure is keyboard reachable"
        );
        assert!(
            !disclosure.is_expanded(),
            "{case} RecoveryState starts with diagnostics collapsed"
        );

        let message_bounds = message
            .compute_bounds(root)
            .expect("RecoveryState message belongs to its document");
        let retry_bounds = retry
            .compute_bounds(root)
            .expect("RecoveryState retry belongs to its document");
        let disclosure_bounds = disclosure
            .compute_bounds(root)
            .expect("RecoveryState disclosure belongs to its document");
        assert!(
            message_bounds.y() + message_bounds.height() <= retry_bounds.y(),
            "{case} RecoveryState retry overlaps its summary"
        );
        assert!(
            retry_bounds.y() + retry_bounds.height() <= disclosure_bounds.y(),
            "{case} RecoveryState disclosure overlaps its retry"
        );
        assert!(
            message_bounds.x() >= 0.0 && message_bounds.x() + message_bounds.width() <= root.width() as f32,
            "{case} RecoveryState summary escapes the component document"
        );

        let partial_summary = "1 container snapshot unavailable; available rows remain visible.";
        let partial_message = find::<gtk::Box>(root, |candidate| {
            candidate.has_css_class("hl-inlinemessage")
                && descendants::<gtk::Label>(candidate.upcast_ref())
                    .iter()
                    .any(|label| label.text() == partial_summary)
        });
        assert_eq!(partial_message.accessible_role(), gtk::AccessibleRole::Alert);
        assert!(partial_message.has_css_class("tone-warning"));
        let mut disclosures = descendants::<gtk::Expander>(root)
            .into_iter()
            .filter(|expander| expander.label().as_deref() == Some("Technical details"))
            .collect::<Vec<_>>();
        disclosures.sort_by(|left, right| {
            left.compute_bounds(root)
                .expect("disclosure belongs to RecoveryState document")
                .y()
                .total_cmp(
                    &right
                        .compute_bounds(root)
                        .expect("disclosure belongs to RecoveryState document")
                        .y(),
                )
        });
        assert_eq!(disclosures.len(), 2, "RecoveryState documents both variants");
        let partial_disclosure = disclosures[1].clone();
        assert_eq!(partial_disclosure.accessible_role(), gtk::AccessibleRole::Button);
        assert!(partial_disclosure.is_focusable());
        assert!(!partial_disclosure.is_expanded());
        let partial_bounds = partial_message
            .compute_bounds(root)
            .expect("partial RecoveryState message belongs to its document");
        let partial_disclosure_bounds = partial_disclosure
            .compute_bounds(root)
            .expect("partial RecoveryState disclosure belongs to its document");
        assert!(
            partial_bounds.y() + partial_bounds.height() <= partial_disclosure_bounds.y(),
            "{case} partial RecoveryState disclosure overlaps its warning"
        );
        assert!(
            partial_bounds.x() >= 0.0 && partial_bounds.x() + partial_bounds.width() <= root.width() as f32,
            "{case} partial RecoveryState summary escapes the component document"
        );
    }

    fn assert_toggle_receipt(root: &gtk::Widget, text: &str, maximum_height: i32) {
        let labels = descendants::<gtk::Label>(root);
        let caption = labels
            .iter()
            .find(|label| label.text() == text)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "ToggleButton receipt {text:?} was absent; rendered labels: {:?}",
                    labels.iter().map(gtk::Label::text).collect::<Vec<_>>()
                )
            });
        let message = caption
            .parent()
            .expect("ToggleButton receipt has its InlineMessage surface");
        assert_eq!(message.accessible_role(), gtk::AccessibleRole::Alert);
        assert!(
            caption.width() >= 90,
            "ToggleButton receipt {text:?} collapsed to a {}px character column",
            caption.width()
        );
        assert!(
            message.height() <= maximum_height,
            "ToggleButton receipt {text:?} grew to {}px, expected at most {maximum_height}px",
            message.height()
        );
    }

    fn find<T: IsA<gtk::Widget> + gtk::glib::object::Cast + Clone + 'static>(
        root: &gtk::Widget,
        accepts: impl Fn(&T) -> bool,
    ) -> T {
        let mut pending = vec![root.clone()];
        while let Some(widget) = pending.pop() {
            if let Ok(candidate) = widget.clone().downcast::<T>() {
                if accepts(&candidate) {
                    return candidate;
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                pending.push(current);
            }
        }
        panic!("expected GTK interaction widget was not rendered")
    }

    fn descendants<T: IsA<gtk::Widget> + gtk::glib::object::Cast + Clone + 'static>(root: &gtk::Widget) -> Vec<T> {
        let mut pending = vec![root.clone()];
        let mut found = Vec::new();
        while let Some(widget) = pending.pop() {
            if let Ok(candidate) = widget.clone().downcast::<T>() {
                found.push(candidate);
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                pending.push(current);
            }
        }
        found
    }

    fn settle_toolkit() {
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
    }

    fn settle_window_width(window: &gtk::Window, expected: i32) {
        for _ in 0..50 {
            settle_toolkit();
            if window.width() == expected {
                window.set_size_request(-1, -1);
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("window remained {}px wide, expected {expected}px", window.width());
    }

    fn assert_contained(parent: &gtk::Widget, story: &str) {
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
                "{story} overflowed {:?}: child x={} width={}, parent width={}",
                (
                    parent.css_classes(),
                    current.css_classes(),
                    current.downcast_ref::<gtk::Label>().map(gtk::Label::text)
                ),
                allocation.x(),
                allocation.width(),
                parent.width()
            );
            assert_contained(&current, story);
        }
    }

    fn readable_heading(root: &gtk::Widget) -> bool {
        let mut pending = vec![root.clone()];
        while let Some(widget) = pending.pop() {
            if widget.has_css_class("hl-heading")
                && widget
                    .downcast_ref::<gtk::Label>()
                    .is_some_and(|label| !label.text().trim().is_empty())
            {
                return true;
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                pending.push(current);
            }
        }
        false
    }

    fn allocate(root: &gtk::Widget, width: i32, height: i32) {
        root.measure(gtk::Orientation::Horizontal, -1);
        root.measure(gtk::Orientation::Vertical, width);
        root.allocate(width, height, -1, None);
    }

    fn assert_document_horizontally_contained(root: &gtk::Widget, context: &str) {
        let document = descendants::<gtk::ScrolledWindow>(root)
            .into_iter()
            .filter(|scroll| scroll.has_css_class("hl-scroll"))
            .max_by(|left, right| left.vadjustment().upper().total_cmp(&right.vadjustment().upper()))
            .unwrap_or_else(|| panic!("{context} owns a scrolling document viewport"));
        let horizontal = document.hadjustment();
        assert_eq!(
            horizontal.value(),
            0.0,
            "{context} shifted its outer document horizontally"
        );
        assert!(
            horizontal.upper() <= horizontal.page_size() + 1.0,
            "{context} widened its outer document to {}px for a {}px viewport",
            horizontal.upper(),
            horizontal.page_size(),
        );
    }

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crate lives below repository/src/workspaces")
            .to_path_buf()
    }
}
