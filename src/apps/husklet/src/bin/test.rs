use super::remove_workspace;
use super::screens::workspace::overview::{
    filter_workspace_procs, ContainerSummary, ImageSummary, NetworkSummary, VolumesResponse,
};
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
    let observed = std::cell::Cell::new(false);

    remove_workspace(&mut store, "demo", |workspace| {
        assert_eq!(workspace.name, "demo");
        assert!(WorkspaceStore::load(&path).unwrap().get("demo").is_some());
        observed.set(true);
        Ok(())
    })
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

    let error = remove_workspace(&mut store, "demo", |_| Err(std::io::Error::other("still running"))).unwrap_err();

    assert_eq!(error.to_string(), "still running");
    assert!(WorkspaceStore::load(path).unwrap().get("demo").is_some());
}

// A captured `ps -axo pid=,ppid=,etime=,command=` slice: Husklet (43405) with two launcher shells for
// the `general` workspace, one of which (43444) has a guest fork (90001); plus an orphaned launcher
// (16020, ppid 1), an UNRELATED workspace launcher (`ubuntu-dev`), and noise that must be excluded.
const PS: &str = "\
43405     1    01:00:00 ./target-mac/release/husklet
43444 43405       04:12 /Applications/Husklet.app/Contents/MacOS/husklet --worker launch general
45125 43405       00:30 /Applications/Husklet.app/Contents/MacOS/husklet --worker launch general
90001 43444       00:05 /Applications/Husklet.app/Contents/MacOS/husklet --worker launch general
16020     1    02:03:04 /Applications/Husklet.app/Contents/MacOS/husklet --worker launch general
17980     1       10:00 /Applications/Husklet.app/Contents/MacOS/husklet --worker launch ubuntu-dev
55500     1       10:00 /usr/sbin/some-daemon --workspace launch generalizer
99999     1       00:01 grep workspace launch general";

#[test]
fn finds_launchers_and_their_forks() {
    let rows = filter_workspace_procs(PS, "general", "bash");
    let pids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    // Every `general` launcher + the guest fork under 43444, and nothing else.
    assert!(pids.contains(&"43444"), "missing launcher 43444");
    assert!(pids.contains(&"45125"), "missing launcher 45125");
    assert!(pids.contains(&"90001"), "missing guest fork 90001");
    assert!(pids.contains(&"16020"), "missing orphaned launcher 16020");
    assert!(!pids.contains(&"17980"), "must not match ubuntu-dev launcher");
    assert!(!pids.contains(&"55500"), "must not match `generalizer` substring");
    assert!(!pids.contains(&"43405"), "Husklet itself is not a workspace process");
    // The fork is a plain process; launchers are named by shell + uptime (from etime).
    let fork = rows.iter().find(|r| r[0] == "90001").unwrap();
    assert_eq!(fork[2], "process");
    let shell = rows.iter().find(|r| r[0] == "43444").unwrap();
    assert_eq!(shell[2], "bash · up 04:12");
}

#[test]
fn exact_name_match_no_prefix_collision() {
    // `general` must never pull in `general-2`'s launcher.
    let ps = "100 1 00:10 /x/husklet --worker launch general-2\n101 1 00:20 /x/husklet --worker launch general";
    let rows = filter_workspace_procs(ps, "general", "fish");
    let pids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(pids, vec!["101"]);
    assert_eq!(rows[0][2], "fish · up 00:20");
}

#[test]
fn workspace_process_matching_preserves_names_with_spaces() {
    let ps = "100 1 00:10 /x/husklet --worker launch design%20system pane-1\n\
              101 1 00:20 /x/husklet --worker launch design slot";

    let rows = filter_workspace_procs(ps, "design system", "bash");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], "100");
}

#[test]
fn workspace_overview_decodes_resource_fields() {
    let containers: Vec<ContainerSummary> =
        serde_json::from_str(r#"[{"Names":["/web"],"Image":"nginx","Status":"Up"}]"#).unwrap();
    assert_eq!(containers[0].names, ["/web"]);
    assert_eq!(containers[0].image, "nginx");

    let images: Vec<ImageSummary> =
        serde_json::from_str(r#"[{"RepoTags":["ubuntu:24.04"],"Id":"sha256:abc","Size":123}]"#).unwrap();
    assert_eq!(images[0].repo_tags, ["ubuntu:24.04"]);
    assert_eq!(images[0].size, 123);

    let volumes: VolumesResponse = serde_json::from_str(r#"{"Volumes":[{"Name":"cache","Driver":"local"}]}"#).unwrap();
    assert_eq!(volumes.volumes[0].name, "cache");

    let networks: Vec<NetworkSummary> =
        serde_json::from_str(r#"[{"Name":"bridge","Driver":"bridge","Scope":"local"}]"#).unwrap();
    assert_eq!(networks[0].scope, "local");

    assert!(serde_json::from_str::<Vec<ContainerSummary>>(r#"[{"Names":["/web"]}]"#).is_err());
    assert!(serde_json::from_str::<Vec<ImageSummary>>(r#"[{"RepoTags":[]}]"#).is_err());
}

#[test]
fn extracts_engine_guest_command() {
    let ps = "100 1 00:10 /x/husklet --worker launch general\n\
                  101 100 00:05 /x/engine --rootfs /tmp/root /usr/bin/python train.py";
    let rows = filter_workspace_procs(ps, "general", "bash");
    let guest = rows.iter().find(|row| row[0] == "101").unwrap();
    assert_eq!(guest[2], "/usr/bin/python train.py");
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
