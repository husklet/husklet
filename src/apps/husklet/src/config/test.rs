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
fn cuda_device_spec_parses_and_roundtrips() {
    let d = CudaDevice::default_device();
    assert_eq!(d.compute_capability, "8.6");
    assert_eq!(CudaDevice::parse(&d.to_spec()).unwrap(), d);
    let full = CudaDevice::parse("My GPU|7.5|8192").unwrap();
    assert_eq!(
        (full.name.as_str(), full.compute_capability.as_str(), full.vram_mb),
        ("My GPU", "7.5", 8192)
    );
    let bare = CudaDevice::parse("JustAName").unwrap();
    assert_eq!(
        (bare.name.as_str(), bare.compute_capability.as_str(), bare.vram_mb),
        ("JustAName", "8.6", 4096)
    );
    assert_eq!(CudaDevice::parse(""), None);
    for malformed in [
        "|8.6|4096",
        "GPU|eight.six|4096",
        "GPU|8.6|nope",
        "GPU|8.6|0",
        "GPU|8.6|4096|extra",
    ] {
        assert_eq!(CudaDevice::parse(malformed), None, "{malformed}");
    }
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
    cfg.gui = true;
    cfg.scrollback = Some(5000);
    cfg.ws.env = vec![("FOO".into(), "bar=baz".into()), ("N".into(), "1".into())];
    cfg.ws.mounts = vec![Mount {
        host: "/h".into(),
        container: "/c".into(),
        ro: true,
    }];
    cfg.vpn = Some(VpnConfig::socks5("127.30.0.1:1080"));
    cfg.cuda = Some(CudaDevice::parse("hl Metal (CUDA-sim) Device|8.6|16384").unwrap());
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

    assert!(store
        .upsert(WorkspaceConfig::new("new", "debian:bookworm", Arch::Arm64))
        .is_err());
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
