//! Real Storybook process → socket protocol → retained tree → GTK adapter.

#[cfg(unix)]
mod unix {
    use std::io::Read as _;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use gtk::prelude::*;
    use hl_extension::{
        codec, Capability, ChannelId, ExtensionName, Frame, Grant, Hello, Kind, Reply, Request, Welcome, Wire, PROTOCOL,
    };
    use hl_gui::{Renderer as _, SourceMutation, Tree, LOG_VIEW_CHARACTER_LIMIT};
    use hl_gui_gtk::Surface;

    const STORIES: &[&str] = &[
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

    #[test]
    fn every_composed_story_crosses_the_real_socket_and_renders_narrow_and_wide() {
        assert!(gtk::init().is_ok(), "run this test under Xvfb");
        let repository = repository();
        assert!(
            repository.join("extensions/node_modules/@husklet/react").exists(),
            "run `npm ci` in extensions before the GTK Storybook E2E"
        );
        for (index, story) in STORIES.iter().enumerate() {
            render_story(&repository, story, index);
        }
    }

    fn render_story(repository: &Path, story: &str, index: usize) {
        let socket = std::env::temp_dir().join(format!("husklet-storybook-{}-{index}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("the Storybook test socket binds");
        let mut child = Command::new("node")
            .arg(repository.join("extensions/storybook/src/main.js"))
            .env("HUSKLET_EXTENSION_SOCKET", &socket)
            .env("HUSKLET_STORYBOOK_STORY", story)
            .current_dir(repository.join("extensions/storybook"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the real Storybook entrypoint starts");
        let (stream, _) = listener.accept().expect("Storybook connects to the host socket");
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
            codec::read_hello(&wire.receive().expect("Storybook greets the host")).expect("hello decodes");
        assert_eq!(hello.protocol, PROTOCOL);

        let mut rendered = None;
        for _ in 0..8 {
            let request = codec::read_request(&wire.receive().expect("Storybook sends a bounded call"))
                .expect("Storybook request decodes through the production codec");
            let reply = match request {
                Request::InterfaceOpenTab { .. } => Reply::Identity("storybook-main".into()),
                Request::InterfaceRenderAt { slot, frame } => {
                    assert_eq!(slot, "storybook-main");
                    assert!(
                        frame.patches.len() <= PATCH_LIMIT,
                        "{story} emitted {} patches",
                        frame.patches.len()
                    );
                    rendered = Some(frame);
                    Reply::Done
                }
                Request::SourceResizeAt { .. } => Reply::Done,
                other => panic!("unexpected Storybook startup call: {other:?}"),
            };
            wire.send(&codec::reply(&reply).expect("reply encodes"))
                .expect("reply is sent");
            if rendered.is_some() {
                break;
            }
        }

        let frame = rendered.unwrap_or_else(|| panic!("{story} never rendered"));
        let mut tree = Tree::new();
        let mut surface = Surface::new();
        tree.apply(&frame, &mut surface)
            .unwrap_or_else(|error| panic!("{story} failed in GTK: {error:?}"));
        if story == "DataTable" {
            loop {
                let carried = wire.receive().expect("DataTable publishes its logical length");
                if carried.kind == Kind::Credit {
                    continue;
                }
                let request = codec::read_request(&carried).expect("source length request decodes");
                let Request::SourceResizeAt { slot, mutation } = request else {
                    panic!("DataTable sent {request:?} before its source length")
                };
                assert_eq!(slot, "storybook-main");
                let SourceMutation::Length { source, version, rows } = mutation else {
                    panic!("DataTable first source mutation was not its length")
                };
                assert_eq!(rows, 100_000);
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
        let _realized_window = (story == "DataTable").then(|| {
            let window = gtk::Window::new();
            window.set_child(Some(&root));
            window.present();
            settle_toolkit();
            window
        });
        for width in [300, 1_200] {
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 1_600, -1, None);
            assert_eq!(root.width(), width, "{story} did not accept the {width}px allocation");
            assert_contained(&root, story);
            if story == "DataTable" && width == 1_200 {
                let panes = descendants::<gtk::ScrolledWindow>(&root)
                    .into_iter()
                    .filter(|scroll| scroll.has_css_class("hl-scroll"))
                    .collect::<Vec<_>>();
                assert_eq!(panes.len(), 2, "Storybook keeps navigation and inspector panes");
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
            let answer = loop {
                let carried = wire.receive().unwrap_or_else(|error| {
                    let mut diagnostic = String::new();
                    child
                        .stderr
                        .as_mut()
                        .expect("captured stderr")
                        .read_to_string(&mut diagnostic)
                        .ok();
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
        let payload = codec::interaction(&event, Some("storybook-main"))
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
        if story == "Extension acquisition" {
            assert!(
                find::<gtk::Label>(&root, |label| label.text() == "Cancel download invoked for checking.")
                    .is_visible(),
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
                let answer = loop {
                    let carried = wire.receive().expect("Storybook answers the accepted-edit window");
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
            let payload = codec::interaction(&stale, Some("storybook-main")).expect("stale edit has a wire representation");
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
            let payload = codec::interaction(&event, Some("storybook-main")).expect("sort has a wire representation");
            wire.send(&Frame::new(ChannelId::new(99), Kind::Event, payload))
                .expect("native sort returns to Node");
            let sorted = receive_rerender(&mut wire, story);
            assert!(
                !sorted.patches.is_empty(),
                "accepted native sort is observable in the story"
            );
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

        child.kill().expect("Storybook test process stops");
        let status = child.wait().expect("Storybook process is reaped");
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("captured stderr")
            .read_to_string(&mut stderr)
            .expect("stderr reads");
        assert!(stderr.is_empty(), "{story} wrote warnings/errors: {stderr}");
        assert!(
            !status.success(),
            "the long-running entrypoint should only end when killed"
        );
        std::fs::remove_file(socket).expect("test socket is removed");
    }

    fn receive_rerender(wire: &mut Wire<std::os::unix::net::UnixStream>, story: &str) -> hl_gui::Frame {
        for _ in 0..8 {
            let carried = wire.receive().expect("Node answers the GTK interaction");
            if carried.kind == Kind::Credit {
                continue;
            }
            let request = codec::read_request(&carried).expect("post-interaction request decodes");
            let frame = match request {
                Request::InterfaceRenderAt { slot, frame } => {
                    assert_eq!(slot, "storybook-main");
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
        for _ in 0..8 {
            let carried = wire.receive().expect("Node answers the stale native edit");
            if carried.kind == Kind::Credit {
                continue;
            }
            match codec::read_request(&carried).expect("stale-edit response decodes") {
                Request::InterfaceRenderAt { slot, frame } => {
                    assert_eq!(slot, "storybook-main");
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
                find::<gtk::Button>(root, |button| button.label().as_deref() == Some("↓ Build")).emit_clicked();
            }
            "Workspace layout control" => {
                find::<gtk::Button>(root, |button| button.label().as_deref() == Some("Open pane chooser"))
                    .emit_clicked();
            }
            "Extension acquisition" => {
                let cancel = find::<gtk::Button>(root, |button| button.label().as_deref() == Some("Cancel download"));
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
                find::<gtk::Button>(root, |button| button.label().as_deref() == Some("Append batch")).emit_clicked();
            }
            "Virtual event timeline" => {
                find::<gtk::Button>(root, |button| button.label().as_deref() == Some("Acknowledge newest"))
                    .emit_clicked();
            }
            "Bounded key/value inspector" => {
                find::<gtk::Button>(root, |button| button.label().as_deref() == Some("Refresh metadata"))
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
        let event = reports.into_iter().next().expect("one report");
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
