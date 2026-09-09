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
        "Button",
        "IconButton",
        "Entry",
        "Select",
        "Switch",
        "ToggleButton",
        "Checkbox",
        "Radio",
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
            "Button" | "Entry" | "Select" | "Checkbox" | "Radio" | "DataTable"
        );
        realized_window.set_default_size(if narrow_story { 600 } else { 1_200 }, 800);
        realized_window.set_child(Some(&root));
        realized_window.present();
        settle_toolkit();
        if narrow_story {
            assert!(realized_window.width() <= 600, "{story} narrow capture remained wide");
            assert_contained(&root, story);
            capture_story(&realized_window, &format!("{story} narrow"));
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            root.allocate(1_200, 800, -1, None);
            settle_toolkit();
        }
        if story == "Button" {
            let search = find::<gtk::Entry>(&root, |entry| {
                entry.placeholder_text().as_deref() == Some("Search components")
            });
            let width = search.width();
            search.set_text("Se");
            root.allocate(1_200, 800, -1, None);
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
            for (class, expected) in [("size-small", 28), ("size-medium", 36), ("size-large", 44)] {
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
            }
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
            assert!(focus.grab_focus(), "Entry accepts deterministic keyboard focus");
            settle_toolkit();
            let _ = surface.reports().drain();
        }
        if story == "Switch" {
            let focus = find::<gtk::Switch>(&root, |switch| {
                switch.tooltip_text().as_deref() == Some("Focused restore switch")
            });
            assert!(focus.grab_focus(), "Switch accepts deterministic keyboard focus");
            focus.set_state_flags(gtk::StateFlags::FOCUSED, false);
            settle_toolkit();
            let _ = surface.reports().drain();
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
            capture_story(&realized_window, "Select open");
            capture_widget(
                &realized_window,
                popover.upcast_ref::<gtk::Widget>(),
                "Select open list",
            );
            live.emit_clicked();
            settle_toolkit();
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
            Some(toggle.is_active())
        } else {
            None
        };
        capture_story(&realized_window, story);
        let responsive = matches!(story, "Button" | "IconButton").then(|| {
            let paned = descendants::<gtk::Paned>(&root)
                .into_iter()
                .next()
                .expect("component documentation owns a responsive shell");
            let body = paned.end_child().expect("wide responsive shell owns the document body");
            (paned, body)
        });
        for width in [300, 1_200] {
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 1_600, -1, None);
            assert_eq!(root.width(), width, "{story} did not accept the {width}px allocation");
            assert_contained(&root, story);
            if let Some((paned, body)) = &responsive {
                let layout = paned.parent().expect("responsive paned remains in its layout");
                let compact = layout.first_child().expect("responsive shell keeps compact navigation");
                if width == 300 {
                    assert!(!paned.is_visible(), "{story} left its desktop sidebar visible at 300px");
                    assert!(compact.is_visible(), "{story} hid its compact selector at 300px");
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
                }
            }
            if story == "DataTable" && width == 1_200 {
                let panes = descendants::<gtk::ScrolledWindow>(&root)
                    .into_iter()
                    .filter(|scroll| scroll.has_css_class("hl-scroll"))
                    .collect::<Vec<_>>();
                assert_eq!(
                    panes.len(),
                    3,
                    "Storybook keeps navigation, document, and bounded API panes",
                );
                assert!(
                    panes.iter().all(|pane| pane.width() >= 150),
                    "wide Storybook panes were clipped to {:?}",
                    panes.iter().map(|pane| pane.width()).collect::<Vec<_>>()
                );
            }
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
                if carried.channel == channel {
                    break carried;
                }
                let follow_up = codec::read_request(&carried).expect("concurrent Storybook call decodes");
                assert!(
                    matches!(
                        follow_up,
                        Request::InterfaceRenderAt { .. } | Request::SourceResizeAt { .. }
                    ),
                    "unexpected call while awaiting row data: {follow_up:?}"
                );
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
            realized_window.set_size_request(600, 800);
            realized_window.set_default_size(600, 800);
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, 600);
            root.allocate(600, 800, -1, None);
            settle_toolkit();
            capture_story(&realized_window, "DataTable narrow");
            realized_window.set_size_request(1_200, 800);
            realized_window.set_default_size(1_200, 800);
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, 1_200);
            root.allocate(1_200, 800, -1, None);
            settle_toolkit();
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

        let event = emit_representative(story, &root, &surface, &tree);
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
        if let Some(before) = toggle_before {
            settle_toolkit();
            let toggle =
                find::<gtk::ToggleButton>(&root, |button| button.tooltip_text().as_deref() == Some("Pin this tab"));
            assert_ne!(
                toggle.is_active(),
                before,
                "documented ToggleButton did not visibly acknowledge its checked-state change"
            );
            capture_story(&realized_window, "ToggleButton unchecked");
        }
        if story == "Checkbox" {
            settle_toolkit();
            let checkbox =
                find::<gtk::CheckButton>(&root, |button| button.label().as_deref() == Some("Include diagnostics"));
            assert!(
                !checkbox.is_active(),
                "controlled Checkbox did not retain Space activation"
            );
            assert!(checkbox.grab_focus(), "controlled Checkbox restores native focus");
            capture_story(&realized_window, "Checkbox focused unchecked");
        }
        if story == "Radio" {
            settle_toolkit();
            let bash = find::<gtk::CheckButton>(&root, |button| button.label().as_deref() == Some("Bash"));
            assert!(bash.is_active(), "controlled Radio did not retain native selection");
            assert!(bash.grab_focus(), "controlled Radio restores native focus");
            assert!(bash.has_focus(), "controlled Radio exposes focus-visible state");
            capture_story(&realized_window, "Radio focused bash");
        }
        if story == "Extension acquisition" {
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
                    if carried.kind != Kind::Credit && carried.channel == channel {
                        break carried;
                    }
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
        root.allocate(300, 1_600, -1, None);
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

    fn emit_representative(story: &str, root: &gtk::Widget, surface: &Surface, tree: &Tree) -> hl_gui::Event {
        match story {
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
            "Select" => {
                let choice =
                    find::<gtk::ToggleButton>(root, |button| button.tooltip_text().as_deref() == Some("Default shell"));
                assert!(choice.grab_focus(), "Select accepts keyboard focus");
            }
            "Switch" => {
                find::<gtk::Switch>(root, |switch| {
                    switch.tooltip_text().as_deref() == Some("Restore panes on launch")
                })
                .set_active(false);
            }
            "ToggleButton" => {
                find::<gtk::ToggleButton>(root, |button| button.tooltip_text().as_deref() == Some("Pin this tab"))
                    .emit_clicked();
            }
            "Checkbox" => {
                let checkbox =
                    find::<gtk::CheckButton>(root, |button| button.label().as_deref() == Some("Include diagnostics"));
                assert!(checkbox.grab_focus(), "Checkbox accepts native keyboard focus");
                assert!(checkbox.has_focus(), "Checkbox exposes its focus-visible state");
                let before = checkbox.is_active();
                checkbox.activate();
                assert_ne!(checkbox.is_active(), before, "Space activation toggles Checkbox state");
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
        let event = if story == "Radio" {
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
        } else {
            reports.into_iter().next().expect("one report")
        };
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

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crate lives below repository/src/workspaces")
            .to_path_buf()
    }
}
