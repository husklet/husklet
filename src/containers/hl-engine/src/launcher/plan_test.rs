use super::*;

fn word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn config() -> LaunchConfig {
    let mut bytes = vec![0; 192 + 8];
    word(&mut bytes, 0, 0x484c_4346);
    word(&mut bytes, 4, 8);
    word(&mut bytes, 8, 192);
    word(&mut bytes, 12, 1);
    word(&mut bytes, 108, 1);
    bytes[152..160].copy_from_slice(&1_u64.to_le_bytes());
    bytes[193..200].copy_from_slice(b"guest\0\0");
    LaunchConfig::parse(&bytes).unwrap()
}

#[test]
fn projects_scalars_credentials() {
    let mut config = config();
    config.memory_limit = 1024;
    config.pid_limit = 7;
    config.cpu_limit = 3;
    config.uid = 1000;
    config.gid = 1001;
    config.process_domain = [0x12, 0x34];
    config.rootfs_read_only = 1;
    config.network_isolated = 1;
    config.network_transport = 1;
    let plan = RuntimePlan::project(&config, DiagnosticsMode::Disabled).unwrap();
    assert_eq!(plan.options.get("HL_MEM_MAX"), Some("1024"));
    assert_eq!(plan.options.get("HL_UID"), Some("1000"));
    assert_eq!(
        plan.options.get("HL_PROCESS_DOMAIN"),
        Some("00000000000000120000000000000034")
    );
    assert_eq!(plan.options.get("HL_NETNS"), plan.options.get("HL_PROCESS_DOMAIN"));
}

#[test]
fn public_domain_can() {
    let domain = crate::Domain::new().expect("create process domain");
    let config = config().domain(domain);

    assert_eq!(config.process_domain, domain.identity());
    assert_ne!(config.process_domain, [0, 0]);
}

#[test]
fn cache_disable_has() {
    let mut config = config();
    config.translation_cache = b"/cache".to_vec();
    config.translation_cache_disabled = 1;
    config.sandbox = 2;
    let plan = RuntimePlan::project(&config, DiagnosticsMode::Disabled).unwrap();
    assert_eq!(plan.options.get("HL_PCACHE"), None);
    assert_eq!(plan.options.get("HL_PCACHE_DIR"), None);
    assert_eq!(plan.options.get("HL_UNTRUSTED"), Some("1"));
    assert_eq!(plan.options.get("HL_SANDBOX"), None);
}

#[test]
fn projects_all_byte() {
    let mut config = config();
    config.rootfs = b"/root".to_vec();
    config.executable_host = b"/host/program".to_vec();
    config.result_path = b"/result".to_vec();
    config.hostname = vec![0xff];
    config.debug_log = b"syscall".to_vec();
    let plan = RuntimePlan::project(&config, DiagnosticsMode::Enabled).unwrap();
    assert_eq!(plan.rootfs.as_deref(), Some(b"/root".as_slice()));
    assert_eq!(plan.options.get_bytes("HL_HOSTNAME"), Some([0xff].as_slice()));
    assert_eq!(plan.options.get("HL_LOG"), Some("syscall"));
}

#[test]
fn formats_publish_and() {
    let mut config = config();
    config.lower_layers = vec![b"/base".to_vec(), b"/app".to_vec()];
    config.publish = vec![
        PortPublication {
            host_ipv4_be: 0,
            host_port: 8080,
            guest_port: 80,
        },
        PortPublication {
            host_ipv4_be: u32::from_le_bytes([127, 0, 0, 1]),
            host_port: 8443,
            guest_port: 443,
        },
    ];
    let plan = RuntimePlan::project(&config, DiagnosticsMode::Disabled).unwrap();
    assert_eq!(plan.options.get("HL_LOWER"), Some("/base\n/app"));
    assert_eq!(plan.options.get("HL_PUBLISH"), Some("8080:80,127.0.0.1:8443:443"));
}
