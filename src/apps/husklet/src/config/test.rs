use super::*;

fn tmp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("hl-store-{}-{}.conf", std::process::id(), tag));
    p
}

#[test]
fn vpn_spec_parses_and_roundtrips() {
    let c = VpnConfig::parse("127.30.0.1:1080").unwrap();
    assert_eq!(c, VpnConfig::socks5("127.30.0.1:1080"));
    assert_eq!(c.socks_endpoint(), Some("127.30.0.1:1080"));
    assert_eq!(VpnConfig::parse("socks5:127.0.0.1:9050").unwrap().kind, VpnKind::Socks5);
    let wg = VpnConfig::parse("wireguard:vpn/wg.conf").unwrap();
    assert_eq!((wg.kind, wg.endpoint.as_str()), (VpnKind::Wireguard, "vpn/wg.conf"));
    assert_eq!(wg.socks_endpoint(), None);
    assert_eq!(VpnConfig::parse(&wg.to_spec()).unwrap(), wg);
    assert_eq!(VpnConfig::parse(""), None);
    assert_eq!(VpnConfig::parse("   "), None);
    for malformed in [
        "not an endpoint",
        "localhost",
        "localhost:0",
        "localhost:65536",
        "localhost:port",
        "socks5:localhost",
        "http::8080",
        "socks5:[::1:1080",
        "socks5:host]:1080",
        "socks5:[host:1080",
        "socks5:bad host:1080",
    ] {
        assert_eq!(VpnConfig::parse(malformed), None, "{malformed}");
    }
    assert_eq!(
        VpnConfig::parse("socks5:[::1]:1080").unwrap().socks_endpoint(),
        Some("[::1]:1080")
    );
}

#[test]
fn store_persists_across_reload() {
    let path = tmp_path("persist");
    let _ = std::fs::remove_file(&path);
    let mut store = WorkspaceStore::load(&path).unwrap();
    assert!(store.all().is_empty());
    store
        .upsert(WorkspaceConfig::new("ubuntu-dev", "ubuntu:24.04", Arch::Arm64))
        .unwrap();
    store
        .upsert(WorkspaceConfig::new("centos", "centos:7", Arch::Amd64))
        .unwrap();
    store
        .upsert(WorkspaceConfig::new("ubuntu-dev", "ubuntu:22.04", Arch::Arm64))
        .unwrap();

    let reloaded = WorkspaceStore::load(&path).unwrap();
    assert_eq!(reloaded.all().len(), 2);
    assert_eq!(reloaded.get("ubuntu-dev").unwrap().image, "ubuntu:22.04");
    assert_eq!(reloaded.get("centos").unwrap().arch, Arch::Amd64);
    let _ = std::fs::remove_file(&path);
}

#[cfg(feature = "runtime")]
#[test]
fn persisted_gui_mutations_publish_once_after_success_and_failures_publish_nothing() {
    let name = format!("gui-lifecycle-{}", std::process::id());
    let path = tmp_path(&name);
    let _ = std::fs::remove_file(&path);
    let before = crate::workspace_lifecycle::revision();
    let mut store = WorkspaceStore::load(&path).unwrap();
    store
        .upsert(WorkspaceConfig::new(&name, "alpine:3.20", Arch::Amd64))
        .unwrap();
    store
        .upsert(WorkspaceConfig::new(&name, "alpine:3.21", Arch::Amd64))
        .unwrap();
    assert!(store.remove(&name).unwrap());
    assert!(!store.remove(&name).unwrap());
    let own: Vec<_> = crate::workspace_lifecycle::since(before)
        .into_iter()
        .filter(|change| change.workspace == name)
        .map(|change| change.action)
        .collect();
    assert_eq!(
        own,
        [
            hl_extension::WorkspaceLifecycleAction::Create,
            hl_extension::WorkspaceLifecycleAction::Update,
            hl_extension::WorkspaceLifecycleAction::Remove,
        ]
    );

    let blocked_parent = std::env::temp_dir().join(format!("lifecycle-blocked-parent-{}", std::process::id()));
    let _ = std::fs::remove_file(&blocked_parent);
    let _ = std::fs::remove_dir(&blocked_parent);
    std::fs::create_dir(&blocked_parent).unwrap();
    let failed_name = format!("failed-{name}");
    let mut blocked = WorkspaceStore::load(blocked_parent.join("workspaces.conf")).unwrap();
    std::fs::remove_dir(&blocked_parent).unwrap();
    std::fs::write(&blocked_parent, b"not a directory").unwrap();
    assert!(blocked
        .upsert(WorkspaceConfig::new(&failed_name, "alpine", Arch::Amd64))
        .is_err());
    assert!(crate::workspace_lifecycle::since(before)
        .into_iter()
        .all(|change| change.workspace != failed_name));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(blocked_parent);
}

#[test]
fn rich_config_roundtrips() {
    let path = tmp_path("rich");
    let _ = std::fs::remove_file(&path);
    let mut cfg = WorkspaceConfig::new("api", "node:20", Arch::Amd64);
    cfg.storage = Some(PathBuf::from("/data/api"));
    cfg.shell = Some("/bin/zsh".into());
    cfg.cpus = Some(4);
    cfg.memory_mb = Some(2048);
    cfg.docker_sock = false;
    cfg.scrollback = Some(5000);
    cfg.ws.env = vec![("FOO".into(), "bar=baz".into()), ("N".into(), "1".into())];
    cfg.ws.mounts = vec![Mount {
        host: "/h".into(),
        container: "/c".into(),
        ro: true,
    }];
    cfg.vpn = Some(VpnConfig::socks5("127.30.0.1:1080"));
    cfg.terminal = TerminalPreferences {
        font_family: Some("Berkeley Mono".into()),
        font_size: Some(14),
        foreground: Some("#f0f0f0".into()),
        background: Some("#101010".into()),
        cursor_shape: Some("beam".into()),
        cursor_blink: Some(false),
    };
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(cfg.clone()).unwrap();

    let got = WorkspaceStore::load(&path).unwrap().get("api").cloned().unwrap();
    assert_eq!(
        got, cfg,
        "rich workspace config should round-trip through the block format"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn legacy_missing_scrollback_migrates_to_the_bounded_default() {
    let path = tmp_path("legacy-scrollback-default");
    std::fs::write(&path, "[workspace]\nname = legacy\nimage = alpine\narch = arm64\n").unwrap();

    let loaded = WorkspaceStore::load(&path).unwrap();
    assert_eq!(loaded.get("legacy").unwrap().scrollback, Some(DEFAULT_SCROLLBACK_LINES));
    let _ = std::fs::remove_file(path);
}

#[test]
fn execution_lifetime_is_backward_compatible_and_nondefault_modes_round_trip_explicitly() {
    let path = tmp_path("execution-lifetime");
    std::fs::write(&path, "[workspace]\nname = legacy\nimage = alpine\narch = amd64\n").unwrap();
    assert_eq!(
        WorkspaceStore::load(&path)
            .unwrap()
            .get("legacy")
            .unwrap()
            .execution_lifetime,
        ExecutionLifetime::Persisted
    );

    let mut workspace = WorkspaceConfig::new("fast", "alpine", Arch::Amd64);
    workspace.execution_lifetime = ExecutionLifetime::Ephemeral;
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(workspace.clone()).unwrap();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("execution_lifetime = ephemeral\n")
    );
    assert_eq!(WorkspaceStore::load(&path).unwrap().get("fast"), Some(&workspace));

    workspace.name = "live".into();
    workspace.execution_lifetime = ExecutionLifetime::Live;
    store.upsert(workspace.clone()).unwrap();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("execution_lifetime = live\n")
    );
    assert_eq!(WorkspaceStore::load(&path).unwrap().get("live"), Some(&workspace));
    let _ = std::fs::remove_file(path);
}

#[test]
fn explicit_unlimited_scrollback_roundtrips_without_becoming_default() {
    let path = tmp_path("unlimited-scrollback");
    let _ = std::fs::remove_file(&path);
    let mut workspace = WorkspaceConfig::new("unlimited", "alpine", Arch::Arm64);
    workspace.scrollback = None;
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(workspace).unwrap();

    let persisted = std::fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("scrollback = unlimited\n"));
    assert_eq!(
        WorkspaceStore::load(&path)
            .unwrap()
            .get("unlimited")
            .unwrap()
            .scrollback,
        None
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn mount_paths_roundtrip_delimiters_and_unicode_through_the_store() {
    let path = tmp_path("mount-path-encoding");
    let _ = std::fs::remove_file(&path);
    let mut workspace = WorkspaceConfig::new("mounts", "ubuntu:24.04", Arch::Arm64);
    workspace.mounts = vec![
        Mount {
            host: "/Users/me/project:archive with spaces/naïve\\source".into(),
            container: "/work:tree/資料\\target".into(),
            ro: false,
        },
        Mount {
            host: "/read:only".into(),
            container: "/guest:readonly".into(),
            ro: true,
        },
    ];
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(workspace.clone()).unwrap();

    let persisted = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        persisted
            .lines()
            .filter(|line| line.starts_with("mount = v2::"))
            .count(),
        2
    );
    let reloaded = WorkspaceStore::load(&path).unwrap();
    assert_eq!(reloaded.get("mounts"), Some(&workspace));
    let _ = std::fs::remove_file(path);
}

#[test]
fn legacy_mount_records_remain_readable() {
    let path = tmp_path("legacy-mount");
    std::fs::write(
        &path,
        concat!(
            "[workspace]\nname = legacy\nimage = alpine\narch = arm64\n",
            "mount = /host/path:/guest/path:ro\n",
            "mount = v2:/guest:rw\n",
        ),
    )
    .unwrap();

    let loaded = WorkspaceStore::load(&path).unwrap();
    assert_eq!(
        loaded.get("legacy").unwrap().mounts,
        [
            Mount {
                host: "/host/path".into(),
                container: "/guest/path".into(),
                ro: true,
            },
            Mount {
                host: "v2".into(),
                container: "/guest".into(),
                ro: false,
            },
        ]
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn versioned_mount_records_reject_malformed_encodings() {
    let path = tmp_path("malformed-versioned-mount");
    for malformed in [
        "v2::F:2F6775657374:rw",
        "v2::GG:2F6775657374:rw",
        "v2::FF:2F6775657374:rw",
        "v2::2F686F7374:2F6775657374:invalid",
        "v2::2F686F7374:2F6775657374:rw:extra",
        "v2:::2F6775657374:rw",
    ] {
        std::fs::write(
            &path,
            format!("[workspace]\nname = malformed\nimage = alpine\narch = arm64\nmount = {malformed}\n"),
        )
        .unwrap();
        let error = WorkspaceStore::load(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{malformed}");
        assert!(error.to_string().contains("line 5"), "{malformed}: {error}");
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn versioned_mount_serialization_is_canonical_across_repeated_saves() {
    let path = tmp_path("canonical-versioned-mount");
    let _ = std::fs::remove_file(&path);
    let mut workspace = WorkspaceConfig::new("canonical", "alpine", Arch::Arm64);
    workspace.mounts.push(Mount {
        host: "/a:b".into(),
        container: "/c\\d".into(),
        ro: false,
    });
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(workspace.clone()).unwrap();
    let first = std::fs::read(&path).unwrap();
    assert!(
        first
            .windows(b"mount = v2::2F613A62:2F635C64:rw\n".len())
            .any(|window| { window == b"mount = v2::2F613A62:2F635C64:rw\n" })
    );

    let mut reloaded = WorkspaceStore::load(&path).unwrap();
    reloaded.upsert(workspace).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), first);
    let _ = std::fs::remove_file(path);
}

#[test]
fn workspace_scrollback_never_wraps_negative_at_the_vte_boundary() {
    let mut workspace = WorkspaceConfig::new("large-scrollback", "alpine", Arch::Arm64);
    workspace.scrollback = Some(i64::MAX as u64);
    assert_eq!(workspace.scrollback_lines(), i64::MAX);
    workspace.scrollback = Some(u64::MAX);
    assert_eq!(workspace.scrollback_lines(), i64::MAX);
}

#[test]
fn store_remove() {
    let path = tmp_path("remove");
    let _ = std::fs::remove_file(&path);
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(WorkspaceConfig::new("a", "alpine", Arch::Arm64)).unwrap();
    assert!(store.remove("a").unwrap());
    assert!(!store.remove("a").unwrap());
    assert!(WorkspaceStore::load(&path).unwrap().all().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn failed_persistence_does_not_change_the_live_store() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store");
    std::fs::create_dir(&path).unwrap();
    let original = WorkspaceConfig::new("original", "ubuntu:24.04", Arch::Arm64);
    let mut store = WorkspaceStore {
        path,
        items: vec![original.clone()],
    };

    assert!(
        store
            .upsert(WorkspaceConfig::new("new", "debian:bookworm", Arch::Arm64))
            .is_err()
    );
    assert_eq!(store.all(), [original]);
}

#[test]
fn malformed_store_is_reported_and_never_replaced() {
    let path = tmp_path("malformed");
    let original = b"[workspace]\nname = runtime\nimage = ubuntu:24.04\narch = nonsense\n";
    std::fs::write(&path, original).unwrap();

    let error = WorkspaceStore::load(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("line 4"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let _ = std::fs::remove_file(path);
}

#[test]
fn store_rejects_unversioned_tab_rows() {
    let path = tmp_path("tab-row");
    let original = b"runtime\tubuntu:24.04\tarm64\n";
    std::fs::write(&path, original).unwrap();

    let error = WorkspaceStore::load(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("line 1"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let _ = std::fs::remove_file(path);
}

#[test]
fn unknown_fields_are_not_silently_destroyed() {
    let path = tmp_path("unknown-field");
    let original = concat!(
        "[workspace]\n",
        "name = runtime\n",
        "image = ubuntu:24.04\n",
        "arch = arm64\n",
        "future_capability = enabled\n",
    );
    std::fs::write(&path, original).unwrap();

    let error = WorkspaceStore::load(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("future_capability"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let _ = std::fs::remove_file(path);
}

#[test]
fn incomplete_workspace_is_not_mistaken_for_an_empty_store() {
    let path = tmp_path("incomplete");
    std::fs::write(&path, "[workspace]\nname = runtime\n").unwrap();

    let error = WorkspaceStore::load(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("missing image"), "{error}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn duplicate_workspace_names_are_rejected() {
    let path = tmp_path("duplicate-name");
    let original = concat!(
        "[workspace]\nname = runtime\nimage = ubuntu:24.04\narch = arm64\n",
        "[workspace]\nname = runtime\nimage = debian:bookworm\narch = arm64\n",
    );
    std::fs::write(&path, original).unwrap();

    let error = WorkspaceStore::load(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("duplicate workspace name"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let _ = std::fs::remove_file(path);
}

#[test]
fn empty_workspace_identity_is_rejected() {
    let path = tmp_path("empty-identity");
    std::fs::write(&path, "[workspace]\nname = \nimage = ubuntu\narch = arm64\n").unwrap();

    let error = WorkspaceStore::load(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("workspace name"), "{error}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn unsupported_control_characters_never_mutate_persisted_values() {
    let path = tmp_path("control-character");
    let _ = std::fs::remove_file(&path);
    let original = WorkspaceConfig::new("runtime", "ubuntu:24.04", Arch::Arm64);
    let mut store = WorkspaceStore::load(&path).unwrap();
    store.upsert(original.clone()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut invalid = WorkspaceConfig::new("other", "ubuntu:24.04", Arch::Arm64);
    invalid.shell = Some("/bin/bash\nmalicious = value".into());

    let error = store.upsert(invalid).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(store.all(), [original]);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let _ = std::fs::remove_file(path);
}
