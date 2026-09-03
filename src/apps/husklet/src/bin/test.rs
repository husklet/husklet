use super::remove_workspace;
use hl::config::{TerminalPreferences, WorkspaceConfig, WorkspaceStore};
use hl_ws::Arch;
use hl_ws_term::CursorShape;

#[test]
fn removal_stops_the_runtime_before_deleting_its_authority() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspaces.conf");
    let mut store = WorkspaceStore::load(&path).unwrap();
    store
        .upsert(WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64))
        .unwrap();
    let generation = store.get("demo").unwrap().generation.clone();
    let observed = std::cell::Cell::new(false);

    remove_workspace(
        &mut store,
        "demo",
        &generation,
        |workspace| {
            assert_eq!(workspace.name, "demo");
            assert!(WorkspaceStore::load(&path).unwrap().get("demo").is_some());
            observed.set(true);
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap();

    assert!(observed.get());
    assert!(WorkspaceStore::load(path).unwrap().get("demo").is_none());
}

#[test]
fn failed_runtime_teardown_preserves_the_workspace_authority() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspaces.conf");
    let mut store = WorkspaceStore::load(&path).unwrap();
    store
        .upsert(WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64))
        .unwrap();
    let generation = store.get("demo").unwrap().generation.clone();

    let error = remove_workspace(
        &mut store,
        "demo",
        &generation,
        |_| Err(std::io::Error::other("still running")),
        |_| Ok(()),
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "still running");
    assert!(WorkspaceStore::load(path).unwrap().get("demo").is_some());
}

#[test]
fn removal_reclaims_launchers_after_domain_teardown_before_deleting_authority() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspaces.conf");
    let mut store = WorkspaceStore::load(&path).unwrap();
    store
        .upsert(WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64))
        .unwrap();
    let generation = store.get("demo").unwrap().generation.clone();
    let events = std::cell::RefCell::new(Vec::new());

    remove_workspace(
        &mut store,
        "demo",
        &generation,
        |_| {
            events.borrow_mut().push("domain");
            Ok(())
        },
        |_| {
            assert!(WorkspaceStore::load(&path).unwrap().get("demo").is_some());
            events.borrow_mut().push("launchers");
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(*events.borrow(), ["domain", "launchers"]);
    assert!(WorkspaceStore::load(path).unwrap().get("demo").is_none());
}

#[test]
fn removal_refuses_a_recreated_workspace_before_runtime_teardown() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspaces.conf");
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(WorkspaceConfig::new("demo", "old", Arch::Arm64)).unwrap();
    let stale = store.get("demo").unwrap().generation.clone();
    store.remove("demo").unwrap();
    store.upsert(WorkspaceConfig::new("demo", "new", Arch::Arm64)).unwrap();
    assert_ne!(store.get("demo").unwrap().generation, stale);

    let teardown_called = std::cell::Cell::new(false);
    let error = remove_workspace(
        &mut store,
        "demo",
        &stale,
        |_| {
            teardown_called.set(true);
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(!teardown_called.get());
    assert_eq!(WorkspaceStore::load(path).unwrap().get("demo").unwrap().image, "new");
}

#[test]
fn removal_atomically_adopts_an_unchanged_legacy_workspace() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspaces.conf");
    std::fs::write(&path, "[workspace]\nname = legacy\nimage = alpine\narch = arm64\n").unwrap();
    let mut store = WorkspaceStore::load(&path).unwrap();
    let teardown_generation = std::cell::RefCell::new(String::new());
    remove_workspace(
        &mut store,
        "legacy",
        "",
        |workspace| {
            teardown_generation.replace(workspace.generation.clone());
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(teardown_generation.borrow().len(), 32);
    assert!(WorkspaceStore::load(path).unwrap().get("legacy").is_none());
}

#[test]
fn workspace_terminal_preferences_are_isolated() {
    let mut workspace = WorkspaceConfig::new("design", "ubuntu:24.04", Arch::Arm64);
    workspace.scrollback = Some(2_000);
    workspace.terminal = TerminalPreferences {
        font_family: Some("Berkeley Mono".into()),
        font_size: Some(15),
        foreground: Some("#fafafa".into()),
        background: Some("#111111".into()),
        cursor_shape: Some("beam".into()),
        cursor_blink: Some(false),
    };

    let terminal = workspace.terminal_config();
    assert_eq!(terminal.font_family, "Berkeley Mono");
    assert!((terminal.font_size - 15.0).abs() < f64::EPSILON);
    assert_eq!(terminal.foreground, "#fafafa");
    assert_eq!(terminal.background, "#111111");
    assert_eq!(terminal.cursor_shape, CursorShape::Beam);
    assert!(!terminal.cursor_blink);
    assert_eq!(terminal.scrollback, Some(2_000));
}
