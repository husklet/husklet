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
    assert_eq!(
        VpnConfig::parse("socks5:127.0.0.1:9050").unwrap().kind,
        VpnKind::Socks5
    );
    let wg = VpnConfig::parse("wireguard:vpn/wg.conf").unwrap();
    assert_eq!(
        (wg.kind, wg.endpoint.as_str()),
        (VpnKind::Wireguard, "vpn/wg.conf")
    );
    assert_eq!(wg.socks_endpoint(), None);
    assert_eq!(VpnConfig::parse(&wg.to_spec()).unwrap(), wg);
    assert_eq!(VpnConfig::parse(""), None);
    assert_eq!(VpnConfig::parse("   "), None);
}

#[test]
fn cuda_device_spec_parses_and_roundtrips() {
    let d = CudaDevice::default_device();
    assert_eq!(d.compute_capability, "8.6");
    assert_eq!(CudaDevice::parse(&d.to_spec()).unwrap(), d);
    let full = CudaDevice::parse("My GPU|7.5|8192").unwrap();
    assert_eq!(
        (
            full.name.as_str(),
            full.compute_capability.as_str(),
            full.vram_mb
        ),
        ("My GPU", "7.5", 8192)
    );
    let bare = CudaDevice::parse("JustAName").unwrap();
    assert_eq!(
        (
            bare.name.as_str(),
            bare.compute_capability.as_str(),
            bare.vram_mb
        ),
        ("JustAName", "8.6", 4096)
    );
    assert_eq!(CudaDevice::parse(""), None);
}

#[test]
fn store_persists_across_reload() {
    let path = tmp_path("persist");
    let _ = std::fs::remove_file(&path);
    let mut store = WorkspaceStore::load(&path);
    assert!(store.all().is_empty());
    store
        .upsert(WorkspaceConfig::new(
            "ubuntu-dev",
            "ubuntu:24.04",
            Arch::Arm64,
        ))
        .unwrap();
    store
        .upsert(WorkspaceConfig::new("legacy", "centos:7", Arch::Amd64))
        .unwrap();
    store
        .upsert(WorkspaceConfig::new(
            "ubuntu-dev",
            "ubuntu:22.04",
            Arch::Arm64,
        ))
        .unwrap();

    let reloaded = WorkspaceStore::load(&path);
    assert_eq!(reloaded.all().len(), 2);
    assert_eq!(reloaded.get("ubuntu-dev").unwrap().image, "ubuntu:22.04");
    assert_eq!(reloaded.get("legacy").unwrap().arch, Arch::Amd64);
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
    let mut store = WorkspaceStore::load(&path);
    store.upsert(cfg.clone()).unwrap();

    let got = WorkspaceStore::load(&path).get("api").cloned().unwrap();
    assert_eq!(
        got, cfg,
        "rich workspace config should round-trip through the block format"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn legacy_tab_format_still_loads() {
    let path = tmp_path("legacy-tab");
    std::fs::write(&path, "# old\nubuntu-dev\tarm64\tubuntu:24.04\n").unwrap();
    let store = WorkspaceStore::load(&path);
    let w = store.get("ubuntu-dev").unwrap();
    assert_eq!(w.image, "ubuntu:24.04");
    assert_eq!(w.arch, Arch::Arm64);
    assert!(w.docker_sock, "legacy rows default docker_sock on");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn store_remove() {
    let path = tmp_path("remove");
    let _ = std::fs::remove_file(&path);
    let mut store = WorkspaceStore::load(&path);
    store
        .upsert(WorkspaceConfig::new("a", "alpine", Arch::Arm64))
        .unwrap();
    assert!(store.remove("a").unwrap());
    assert!(!store.remove("a").unwrap());
    assert!(WorkspaceStore::load(&path).all().is_empty());
    let _ = std::fs::remove_file(&path);
}
