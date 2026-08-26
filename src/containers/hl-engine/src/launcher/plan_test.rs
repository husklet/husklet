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
    assert_eq!(plan.box_policy.flags, 0b101);
    assert_eq!((plan.box_policy.uid, plan.box_policy.gid), (1000, 1001));
    assert_eq!(
        plan.box_policy.network_namespace.as_deref(),
        plan.options.get_bytes("HL_NETNS")
    );
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
    assert_eq!(plan.box_policy.flags, 0b11_0000);
    assert_eq!(plan.box_policy.translation_cache.as_deref(), Some(b"/cache".as_slice()));
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
    assert_eq!(plan.box_policy.hostname.as_deref(), Some([0xff].as_slice()));
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
    assert_eq!(plan.box_policy.lower_layers.as_deref(), Some(b"/base\n/app".as_slice()));
    assert_eq!(plan.box_policy.publish, config.publish);
}

/// Host ownership of the plan's writable root, which decides whether a launch can write at all.
#[cfg(unix)]
mod rootfs_ownership {
    use crate::launcher::plan::RuntimePlan;
    use crate::options::Options;

    fn plan(rootfs: Option<&str>, options: &[(&str, &str)]) -> RuntimePlan {
        let mut set = Options::default();
        for (name, value) in options {
            set.set(name, value, true).unwrap();
        }
        RuntimePlan {
            rootfs: rootfs.map(|path| path.as_bytes().to_vec()),
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options: set,
            box_policy: Default::default(),
        }
    }

    /// `/` is owned by host root on every host this runs on, so for an engine that is not root it is
    /// the portable stand-in for a rootfs unpacked under `sudo` -- and it needs no privilege to set
    /// up. It says nothing when the suite itself runs as root, which is the case the test below
    /// covers separately.
    fn host_root_owned() -> &'static str {
        "/"
    }

    /// `refuse_unownable_root` has two acceptance branches and one refusal, and which of them a run
    /// exercises is decided by the identity the suite happens to have. The root arm used to be a
    /// bare `return;`, so on a host that runs the suite as uid 0 -- as this repository's Linux box
    /// does -- the case asserted nothing and reported `ok`, and `engine_uid == 0` had no coverage on
    /// any host. Assert the branch this identity actually reaches instead of leaving the run empty.
    ///
    /// `/` cannot express the root arm, because root owns it and the `rootfs_uid == engine_uid` arm
    /// would answer first. The root branch is a pure uid decision, so exercise it with a foreign uid
    /// directly instead of requiring `CAP_CHOWN`; Nix intentionally runs uid 0 without that capability.
    #[test]
    fn a_writable_root_owned_by_another_host_user_refuses_a_launch_that_is_not_root() {
        assert!(!super::root_is_unownable(0, 65_534));
        assert!(super::root_is_unownable(1_000, 0));
        assert!(!super::root_is_unownable(1_000, 1_000));
    }

    /// The kinder-to-refuse judgement stops exactly where the workspace stops being broken. A
    /// read-only launch writes nothing, so a root-owned tree serves it perfectly well and refusing
    /// it would take away a shape that works.
    #[test]
    fn a_read_only_launch_over_the_same_root_is_still_admitted() {
        assert_eq!(
            plan(Some(host_root_owned()), &[("HL_ROOTFS_RO", "1")]).writable_root(),
            None
        );
        assert_eq!(
            plan(Some(host_root_owned()), &[("HL_ROOTFS_RO", "1")]).refuse_unownable_root(),
            Ok(())
        );
    }

    /// With an overlay the lower layers are read-only by construction and every write lands in the
    /// upper, so the upper is the only ownership that decides whether the workspace works.
    #[test]
    fn an_overlay_is_judged_by_its_upper_layer_not_by_a_root_owned_lower() {
        let directory = tempfile::tempdir().unwrap();
        let upper = directory.path().to_str().unwrap();
        let over_root = plan(Some(host_root_owned()), &[("HL_OVERLAY_UPPER", upper)]);
        assert_eq!(over_root.writable_root(), Some(upper.as_bytes()));
        assert_eq!(over_root.refuse_unownable_root(), Ok(()));
    }

    #[test]
    fn a_root_the_engine_owns_and_a_launch_without_one_are_both_admitted() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            plan(Some(directory.path().to_str().unwrap()), &[]).refuse_unownable_root(),
            Ok(())
        );
        assert_eq!(plan(None, &[]).refuse_unownable_root(), Ok(()));
    }

    /// A rootfs that is not there is the existing launch path's error to report, and an ownership
    /// cause for a missing directory would be a worse diagnostic than the one it replaced.
    #[test]
    fn a_missing_root_is_left_to_the_launch_path_that_already_owns_it() {
        assert_eq!(
            plan(Some("/var/tmp/husklet-no-such-rootfs-6f21"), &[]).refuse_unownable_root(),
            Ok(())
        );
    }
}
