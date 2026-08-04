use super::{ApiError, ArchiveQuery, DockerSignal, HostSettings, LogsQuery, NetworkPlan};
use crate::api::{
    DockerMount, EndpointConfig, EndpointIpam, EndpointsConfig, ExposedPorts, HostConfig, LogOptions, NetworkingConfig,
    PortBinding, PortBindings, RestartPolicy,
};
use hl_container::{Access, ContainerSpec, NetworkDriver, NetworkSpec, Process, Signal, Subnet};
use std::collections::BTreeMap;

#[test]
fn parses_docker_signal_names_and_numbers() {
    assert_eq!(
        Signal::from("SIGTERM".parse::<DockerSignal>().unwrap()),
        Signal::Terminate
    );
    assert_eq!(Signal::from("kill".parse::<DockerSignal>().unwrap()), Signal::Kill);
    assert_eq!(Signal::from("2".parse::<DockerSignal>().unwrap()), Signal::Interrupt);
    assert_eq!(Signal::from("SIGQUIT".parse::<DockerSignal>().unwrap()), Signal::Quit);
    assert!("SIGBOGUS".parse::<DockerSignal>().is_err());
}

#[test]
fn archive_copy_ownership_is_explicit_and_unsupported_overwrite_fails() {
    let query = ArchiveQuery {
        path: "/tmp".into(),
        copy_uid_gid: true,
        no_overwrite_dir_non_dir: false,
    };
    assert!(query.extract_ownership().unwrap());
    let query = ArchiveQuery {
        path: "/tmp".into(),
        copy_uid_gid: false,
        no_overwrite_dir_non_dir: true,
    };
    assert_eq!(
        query.extract_ownership().unwrap_err().status,
        axum::http::StatusCode::NOT_IMPLEMENTED
    );
}

async fn containers() -> (tempfile::TempDir, hl_container::Containers) {
    let root = tempfile::tempdir().unwrap();
    let containers = hl_container::Containers::builder(
        hl_container::Config::new(root.path()).persistence(hl_container::Persistence::Memory),
    )
    .build()
    .await
    .unwrap();
    (root, containers)
}

#[tokio::test]
async fn legacy_bind_strings_map_source_target_access_and_anonymous_volume() {
    let (root, containers) = containers().await;
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();

    let (mount, owned) = super::LegacyBind::from(format!("{}:/data:ro", source.display()).as_str())
        .mount(&containers)
        .await
        .unwrap();
    assert_eq!(mount.source, hl_container::MountSource::Bind(source.clone()));
    assert_eq!(mount.target, std::path::PathBuf::from("/data"));
    assert_eq!(mount.access, Access::ReadOnly);
    assert_eq!(owned, None);

    let (mount, owned) = super::LegacyBind::from(format!("{}:/data:rw", source.display()).as_str())
        .mount(&containers)
        .await
        .unwrap();
    assert_eq!(mount.access, Access::ReadWrite);
    assert_eq!(owned, None);

    let (anonymous, owned) = super::LegacyBind::from("/cache").mount(&containers).await.unwrap();
    assert_eq!(anonymous.target, std::path::PathBuf::from("/cache"));
    assert!(matches!(anonymous.source, hl_container::MountSource::Anonymous(_)));
    assert!(owned.is_some());

    assert!(super::LegacyBind::from(":").mount(&containers).await.is_err());
    assert!(
        super::LegacyBind::from("a:b:ro:extra")
            .mount(&containers)
            .await
            .is_err()
    );
    assert!(
        super::LegacyBind::from("source:relative")
            .mount(&containers)
            .await
            .is_err()
    );
    assert!(
        super::LegacyBind::from("source:/safe/../escape")
            .mount(&containers)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn empty_mode_selects_default_bridge_and_explicit_none_stays_isolated() {
    let default = NetworkPlan::from_request(Some(&HostConfig::default()), None).unwrap();
    assert_eq!(default.attachments[0].name, "bridge");
    assert!(!default.isolated());

    let none = NetworkPlan::from_request(
        Some(&HostConfig {
            network_mode: "none".into(),
            ..HostConfig::default()
        }),
        None,
    )
    .unwrap();
    assert_eq!(none.attachments[0].name, "none");

    let (_root, containers) = containers().await;
    NetworkPlan::ensure_bridge(&containers).await.unwrap();
    let bridge = containers.networks().inspect("bridge").await.unwrap();
    assert_eq!(bridge.driver, hl_container::NetworkDriver::Bridge);
    assert_eq!(bridge.driver, NetworkDriver::Bridge);

    containers.networks().create(NetworkSpec::none("airgap")).await.unwrap();
    let custom = NetworkPlan::from_request(
        Some(&HostConfig {
            network_mode: "airgap".into(),
            ..HostConfig::default()
        }),
        None,
    )
    .unwrap()
    .prepare(&containers)
    .await
    .unwrap();
    assert!(custom.prepare(&containers).await.unwrap().isolated());
}

#[tokio::test]
async fn published_automatic_container_is_inspectable_on_default_bridge() {
    let (root, containers) = containers().await;
    let plan = NetworkPlan::from_request(Some(&HostConfig::default()), None)
        .unwrap()
        .prepare(&containers)
        .await
        .unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let publish = hl_container::Publication::tcp("127.0.0.1".parse().unwrap(), 18083, 8083).unwrap();
    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/true"))
                .name("published-default")
                .isolation(hl_container::Isolation {
                    network_isolated: false,
                    ..hl_container::Isolation::default()
                })
                .publish(publish),
        )
        .await
        .unwrap();
    plan.attach_created(&containers, "published-default").await.unwrap();

    let owner = containers.inspect("published-default").await.unwrap();
    assert_eq!(owner.spec.publish, vec![publish]);
    let bridge = containers.networks().inspect("bridge").await.unwrap();
    assert!(bridge.endpoints.contains_key(&owner.id));
}

#[tokio::test]
async fn host_mode_has_no_endpoints_and_discards_publications() {
    let host = HostConfig {
        network_mode: "host".into(),
        port_bindings: PortBindings(BTreeMap::from([(
            "8080/tcp".into(),
            Some(vec![PortBinding {
                host_ip: "127.0.0.1".into(),
                host_port: "18080".into(),
            }]),
        )])),
        ..HostConfig::default()
    };
    let plan = NetworkPlan::from_request(Some(&host), None).unwrap();
    assert_eq!(plan.mode(), hl_container::NetworkMode::Host);
    assert!(plan.attachments.is_empty());
    let conflicting = NetworkingConfig {
        endpoints_config: EndpointsConfig(BTreeMap::from([("custom".into(), EndpointConfig::default())])),
    };
    assert!(NetworkPlan::from_request(Some(&host), Some(&conflicting)).is_err());
    let linked = HostConfig {
        links: vec!["db:db".into()],
        ..host.clone()
    };
    assert!(NetworkPlan::from_request(Some(&linked), None).is_err());

    let (_root, containers) = containers().await;
    let settings = HostSettings::parse(
        Some(&host),
        &ExposedPorts(BTreeMap::from([("80/tcp".into(), serde_json::json!({}))])),
        BTreeMap::new(),
        false,
        hl_container::NetworkMode::Host,
        &containers,
    )
    .await
    .unwrap();
    assert_eq!(settings.network_mode, hl_container::NetworkMode::Host);
    assert_eq!(settings.isolation.sandbox, hl_container::Sandbox::Disabled);
    assert_eq!(settings.ports.len(), 1);
    assert!(settings.publish.is_empty());
}

#[tokio::test]
async fn custom_bridge_attaches_and_failed_ipam_rolls_back_container() {
    let (root, containers) = containers().await;
    let subnet = Subnet::new("10.77.0.0".parse().unwrap(), 24).unwrap();
    containers
        .networks()
        .create(NetworkSpec::bridge("custom", subnet))
        .await
        .unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let first = containers
        .create(ContainerSpec::from_directory(&rootfs, Process::new("/bin/true")).name("first"))
        .await
        .unwrap();
    let plan = NetworkPlan::from_request(
        Some(&HostConfig {
            network_mode: "custom".into(),
            ..HostConfig::default()
        }),
        None,
    )
    .unwrap();
    plan.attach_created(&containers, first.id.as_str()).await.unwrap();
    assert_eq!(
        containers.networks().inspect("custom").await.unwrap().endpoints.len(),
        1
    );

    let second = containers
        .create(ContainerSpec::from_directory(rootfs, Process::new("/bin/true")).name("second"))
        .await
        .unwrap();
    let plan = NetworkPlan::from_request(
        Some(&HostConfig {
            network_mode: "custom".into(),
            ..HostConfig::default()
        }),
        Some(&NetworkingConfig {
            endpoints_config: EndpointsConfig(BTreeMap::from([(
                "custom".into(),
                EndpointConfig {
                    ipam: Some(EndpointIpam {
                        ipv4_address: "10.77.0.1".into(),
                        ..EndpointIpam::default()
                    }),
                    ..EndpointConfig::default()
                },
            )])),
        }),
    )
    .unwrap();
    assert!(plan.attach_created(&containers, second.id.as_str()).await.is_err());
    assert!(containers.inspect(second.id.as_str()).await.is_err());
}

#[tokio::test]
async fn host_config_maps_effective_resources_isolation_and_mounts() {
    let value = HostConfig {
        binds: vec!["/host/data:/data:ro".into()],
        mounts: vec![DockerMount {
            kind: "bind".into(),
            source: "/host/cache".into(),
            target: "/cache".into(),
            read_only: true,
            bind_options: Some(crate::api::BindOptions {
                propagation: "private".into(),
                read_only: crate::api::BindReadOnly {
                    read_only_force_recursive: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
            volume_options: None,
            unsupported: BTreeMap::new(),
        }],
        tmpfs: BTreeMap::new(),
        extra_hosts: vec!["database=203.0.113.9".into()],
        memory: 64 * 1024 * 1024,
        pids_limit: Some(32),
        nano_cpus: 1_500_000_000,
        readonly_rootfs: true,
        network_mode: "host".into(),
        links: Vec::new(),
        restart_policy: RestartPolicy::default(),
        auto_remove: true,
        port_bindings: PortBindings::default(),
        unsupported: BTreeMap::new(),
    };
    let (_root, containers) = containers().await;
    let settings = HostSettings::parse(
        Some(&value),
        &ExposedPorts::default(),
        BTreeMap::new(),
        false,
        hl_container::NetworkMode::Host,
        &containers,
    )
    .await
    .unwrap();
    assert_eq!(settings.resources.memory_bytes, 64 * 1024 * 1024);
    assert_eq!(settings.resources.process_count, 32);
    assert_eq!(settings.resources.cpu_count, 2);
    assert_eq!(settings.hosts.get("database"), Some(&"203.0.113.9".parse().unwrap()));
    assert!(settings.isolation.read_only_root);
    assert!(!settings.isolation.network_isolated);
    assert_eq!(settings.removal, hl_container::RemovalPolicy::Automatic);
    assert_eq!(settings.mounts.len(), 2);
    assert_eq!(settings.mounts[0].access, Access::ReadOnly);
    assert_eq!(settings.mounts[1].access, Access::ReadOnly);
    assert_eq!(settings.mounts[1].propagation, hl_container::BindPropagation::Private);
}

#[tokio::test]
async fn host_config_rejects_values_the_runtime_cannot_honor() {
    let (_root, containers) = containers().await;
    let mut value = HostConfig {
        network_mode: "host".into(),
        ..HostConfig::default()
    };
    let host = NetworkPlan::from_request(Some(&value), None).unwrap();
    assert_eq!(host.mode(), hl_container::NetworkMode::Host);
    assert!(host.attachments.is_empty());

    value.network_mode.clear();
    value
        .unsupported
        .insert("Privileged".into(), serde_json::Value::Bool(true));
    assert_not_implemented(
        &HostSettings::parse(
            Some(&value),
            &ExposedPorts::default(),
            BTreeMap::new(),
            true,
            hl_container::NetworkMode::Automatic,
            &containers,
        )
        .await
        .unwrap_err(),
    );

    value
        .unsupported
        .insert("Privileged".into(), serde_json::Value::Bool(false));
    assert!(
        HostSettings::parse(
            Some(&value),
            &ExposedPorts::default(),
            BTreeMap::new(),
            true,
            hl_container::NetworkMode::Automatic,
            &containers,
        )
        .await
        .is_ok()
    );
}

#[test]
fn log_query_parses_docker_time_tail_and_stream_selectors() {
    let options = LogOptions::try_from(LogsQuery {
        stdout: Some("true".into()),
        stderr: Some("false".into()),
        follow: Some("1".into()),
        since: Some("1.250".into()),
        until: Some("1970-01-01T00:00:02Z".into()),
        timestamps: Some("true".into()),
        tail: Some("7".into()),
        details: None,
        unsupported: BTreeMap::new(),
    })
    .unwrap();
    assert!(options.follow);
    assert!(options.streams.stdout);
    assert!(!options.streams.stderr);
    assert_eq!(options.since_ms, Some(1_250));
    assert_eq!(options.until_ms, Some(2_000));
    assert_eq!(options.tail, Some(7));
    assert!(options.timestamps);
    for tail in [
        None,
        Some(""),
        Some("all"),
        Some("invalid"),
        Some(" "),
        Some("9223372036854775808"),
        Some("-9"),
    ] {
        assert_eq!(
            LogOptions::try_from(LogsQuery {
                stdout: Some("true".into()),
                tail: tail.map(str::to_owned),
                ..LogsQuery::default()
            })
            .unwrap()
            .tail,
            None
        );
    }
    assert_eq!(
        LogOptions::try_from(LogsQuery {
            stdout: Some("true".into()),
            tail: Some("0".into()),
            ..LogsQuery::default()
        })
        .unwrap()
        .tail,
        Some(0)
    );
    assert!(
        LogOptions::try_from(LogsQuery {
            unsupported: BTreeMap::from([("compress".into(), "true".into())]),
            ..LogsQuery::default()
        })
        .is_err()
    );
}

fn assert_not_implemented(error: &ApiError) {
    assert_eq!(error.status, axum::http::StatusCode::NOT_IMPLEMENTED);
}
