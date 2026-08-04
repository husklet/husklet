use super::*;

fn sample_session() -> Session {
    Session {
        tabs: vec![
            SessionTab {
                title: "shell 1".to_string(),
                root: PaneNode::Leaf(Pane {
                    cwd: Some("/root/my project".to_string()),
                    history_file: Some("hist-0.txt".to_string()),
                    slot: Some("0".to_string()),
                }),
            },
            SessionTab {
                title: "build".to_string(),
                root: PaneNode::Split {
                    dir: SplitDir::Horizontal,
                    ratio: 0.5,
                    a: Box::new(PaneNode::Leaf(Pane {
                        cwd: None,
                        history_file: None,
                        slot: Some("1".to_string()),
                    })),
                    b: Box::new(PaneNode::Split {
                        dir: SplitDir::Vertical,
                        ratio: 0.3,
                        a: Box::new(PaneNode::Leaf(Pane {
                            cwd: Some("/tmp".to_string()),
                            history_file: None,
                            slot: Some("2".to_string()),
                        })),
                        b: Box::new(PaneNode::leaf()),
                    }),
                },
            },
        ],
    }
}

#[test]
fn layout_roundtrips() {
    let s = sample_session();
    let text = s.serialize();
    let back = Session::parse(&text).unwrap();
    assert_eq!(back.tabs.len(), 2);
    assert_eq!(back.tabs[0].title, "shell 1");
    // ratio is formatted to 4 decimals; compare the structure with tolerance.
    assert_eq!(back.tabs[0].root, s.tabs[0].root);
    assert_eq!(back.tabs[1].root, s.tabs[1].root);
    // Each pane's layout slot round-trips.
    assert_eq!(back.tabs[0].root.leaves()[0].slot.as_deref(), Some("0"));
    assert_eq!(back.tabs[1].root.leaves()[0].slot.as_deref(), Some("1"));
    assert_eq!(back.tabs[1].root.leaves()[1].slot.as_deref(), Some("2"));
}

#[test]
fn escaping_survives_spaces_and_specials() {
    let s = Session {
        tabs: vec![SessionTab {
            title: "a b%c".to_string(),
            root: PaneNode::Leaf(Pane {
                cwd: Some("/p a/th".to_string()),
                history_file: None,
                slot: None,
            }),
        }],
    };
    let back = Session::parse(&s.serialize()).unwrap();
    assert_eq!(back.tabs[0].title, "a b%c");
    assert_eq!(back.tabs[0].root.leaves()[0].cwd.as_deref(), Some("/p a/th"));
}

#[test]
fn empty_and_absent_are_empty() {
    assert!(Session::parse("").is_err());
    assert_eq!(Session::parse("# just a comment\nversion 1\n").unwrap().tabs.len(), 0);
}

#[test]
fn parser_rejects_the_whole_malformed_layout() {
    let result = Session::parse("version 1\ntab valid leaf /ok hist 7\ntab broken hsplit nope leaf /a -\n");
    assert!(result.is_err());
}

#[test]
fn malformed_percent_escape_is_preserved() {
    let session = Session::parse("version 1\ntab bad%zz leaf /tmp - -").unwrap();
    assert_eq!(session.tabs[0].title, "bad%zz");
}

#[test]
fn open_reports_corrupt_persistent_layouts() {
    let temporary = tempfile::tempdir().unwrap();
    let session = Session::dir(temporary.path());
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(session.join("layout.conf"), "version 1\ntab broken hsplit nope\n").unwrap();

    let error = Session::open(temporary.path()).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn leaves_are_left_to_right() {
    let s = sample_session();
    let leaves = s.tabs[1].root.leaves();
    assert_eq!(leaves.len(), 3);
    assert_eq!(leaves[1].cwd.as_deref(), Some("/tmp"));
}

#[test]
fn save_load_via_disk() {
    let dir = std::env::temp_dir().join(format!("hl-sess-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s = sample_session();
    s.save(&dir).unwrap();
    let back = Session::open(&dir).unwrap();
    assert_eq!(back, s);
    Session::clear(&dir).unwrap();
    assert_eq!(Session::open(&dir).unwrap(), Session::default());
}

#[test]
fn successful_layout_commit_prunes_only_unreferenced_histories() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = Session::dir(temporary.path());
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("hist-old.txt"), "old").unwrap();
    std::fs::write(directory.join("hist-current.txt"), "current").unwrap();
    std::fs::write(directory.join("unrelated"), "keep").unwrap();
    let session = Session {
        tabs: vec![SessionTab {
            title: "shell".into(),
            root: PaneNode::Leaf(Pane {
                history_file: Some("hist-current.txt".into()),
                ..Pane::default()
            }),
        }],
    };

    session.save(temporary.path()).unwrap();

    assert!(!directory.join("hist-old.txt").exists());
    assert_eq!(
        std::fs::read_to_string(directory.join("hist-current.txt")).unwrap(),
        "current"
    );
    assert_eq!(std::fs::read_to_string(directory.join("unrelated")).unwrap(), "keep");
}

#[test]
fn history_paths_cannot_escape_or_address_unrelated_files() {
    let storage = Path::new("/workspace");
    assert_eq!(
        Session::history_path(storage, "hist-generation-0.txt").unwrap(),
        storage.join("session/hist-generation-0.txt")
    );
    for invalid in [
        "../secret",
        "hist-../secret.txt",
        "/tmp/hist-secret.txt",
        "nested/hist-secret.txt",
        "layout.conf",
        "hist-no-extension",
    ] {
        let error = Session::history_path(storage, invalid).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{invalid}");
    }
}

#[test]
fn persistent_layout_rejects_unsafe_history_references() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = Session::dir(temporary.path());
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("layout.conf"),
        "version 1\ntab shell leaf /root ../secret slot\n",
    )
    .unwrap();

    let error = Session::open(temporary.path()).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("history filename"), "{error}");
}

#[test]
fn replay_bytes_normalizes() {
    let bytes = History::new("line one\nline two\n\n\n").replay();
    let s = String::from_utf8(bytes).unwrap();
    assert!(s.contains("line one\r\nline two\r\n"));
    assert!(!s.contains("\n\n\n")); // trailing blanks trimmed
    assert!(History::new("   \n\n").replay().is_empty());
}

#[test]
fn clamp_keeps_most_recent() {
    let text = (0..100).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
    let clamped = History::new(&text).clamp(10);
    let lines: Vec<&str> = clamped.split('\n').collect();
    assert_eq!(lines.len(), 10);
    assert_eq!(lines[0], "90");
    assert_eq!(lines[9], "99");
}

#[test]
fn cwd_uri_decoding() {
    assert_eq!(
        WorkingDirectory::from_osc7("file://host/root/my%20dir")
            .map(|path| path.into_string())
            .as_deref(),
        Some("/root/my dir")
    );
    assert_eq!(
        WorkingDirectory::from_osc7("file:///tmp/x")
            .map(|path| path.into_string())
            .as_deref(),
        Some("/tmp/x")
    );
    assert_eq!(WorkingDirectory::from_osc7("http://x/y"), None);
}
