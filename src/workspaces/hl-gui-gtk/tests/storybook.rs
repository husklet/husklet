//! Real Storybook process → socket protocol → retained tree → GTK adapter.

#[cfg(unix)]
mod unix {
    use std::io::Read as _;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use gtk::prelude::*;
    use hl_extension::{Capability, ExtensionName, Grant, Hello, PROTOCOL, Reply, Request, Welcome, Wire, codec};
    use hl_gui::Tree;
    use hl_gui_gtk::Surface;

    const STORIES: &[&str] = &[
        "Extension acquisition",
        "Validated settings form",
        "Keyboard and semantic actions",
        "DataTable",
        "Navigation and transient UI",
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
        let root = surface.widget().clone().upcast::<gtk::Widget>();
        for width in [300, 1_200] {
            root.measure(gtk::Orientation::Horizontal, -1);
            root.measure(gtk::Orientation::Vertical, width);
            root.allocate(width, 1_600, -1, None);
            assert_eq!(root.width(), width, "{story} did not accept the {width}px allocation");
            assert_contained(&root, story);
        }
        assert!(readable_heading(&root), "{story} has no readable GTK heading");

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
                (parent.css_classes(), current.css_classes(), current.downcast_ref::<gtk::Label>().map(gtk::Label::text)),
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
