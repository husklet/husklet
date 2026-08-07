use super::*;

#[tokio::test]
async fn none_and_bridge_networks_validate_isolation_before_launch() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = service(Arc::clone(&runtime)).await;
    containers
        .networks()
        .create(NetworkSpec::none("isolated"))
        .await
        .unwrap();
    containers
        .networks()
        .create(NetworkSpec::bridge(
            "bridge-test",
            Subnet::new("10.90.0.0".parse().unwrap(), 24).unwrap(),
        ))
        .await
        .unwrap();
    containers
        .create(spec("none-owner").isolation(Isolation {
            network_isolated: true,
            ..Isolation::default()
        }))
        .await
        .unwrap();
    containers
        .create(spec("bridge-owner").isolation(Isolation {
            network_isolated: false,
            ..Isolation::default()
        }))
        .await
        .unwrap();
    containers
        .create(spec("conflict").isolation(Isolation {
            network_isolated: false,
            ..Isolation::default()
        }))
        .await
        .unwrap();
    containers
        .networks()
        .connect("isolated", "none-owner", EndpointSpec::default())
        .await
        .unwrap();
    containers
        .networks()
        .connect("bridge-test", "bridge-owner", EndpointSpec::default())
        .await
        .unwrap();
    containers
        .networks()
        .connect("isolated", "conflict", EndpointSpec::default())
        .await
        .unwrap();

    containers.start("none-owner").await.unwrap();
    assert!(matches!(
        containers.networks().disconnect("isolated", "none-owner").await,
        Err(Error::InvalidState { .. })
    ));
    containers.wait("none-owner").await.unwrap();
    containers.start("bridge-owner").await.unwrap();
    containers.wait("bridge-owner").await.unwrap();
    assert!(matches!(
        containers.start("conflict").await,
        Err(Error::InvalidSpec(message)) if message.contains("network isolation")
    ));
    assert!(matches!(
        containers.inspect("bridge-owner").await.unwrap().state,
        ContainerState::Exited {
            result: ExitStatus::Code(0),
            ..
        }
    ));
    assert_eq!(
        containers.inspect("conflict").await.unwrap().state,
        ContainerState::Created
    );
    assert_eq!(runtime.mounts.lock().unwrap().len(), 2);
    let owner = containers.inspect("bridge-owner").await.unwrap();
    let recorded = runtime.mounts.lock().unwrap();
    let hosts = |launch: usize| {
        let source = &recorded[launch]
            .iter()
            .find(|mount| mount.1 == std::path::Path::new("/etc/hosts"))
            .expect("hosts mount")
            .0;
        std::fs::read_to_string(source).unwrap()
    };
    assert_eq!(
        hosts(0),
        "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\nfe00::\tip6-localnet\nff00::\tip6-mcastprefix\nff02::1\tip6-allnodes\nff02::2\tip6-allrouters\n"
    );
    assert_eq!(
        hosts(1),
        format!(
            "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\nfe00::\tip6-localnet\nff00::\tip6-mcastprefix\nff02::1\tip6-allnodes\nff02::2\tip6-allrouters\n10.90.0.2\tbridge-owner {}\n",
            &owner.id.as_str()[..12]
        )
    );
    drop(recorded);
    let launches = runtime.networks.lock().unwrap();
    assert_eq!(launches.len(), 2);
    assert!(launches[0][0].bridge.is_none());
    let bridge = &launches[1][0];
    assert_ne!(bridge.namespace, bridge.bridge.as_ref().unwrap().as_str());
    assert_eq!(bridge.address, Some("10.90.0.2".parse().unwrap()));
}

#[tokio::test]
async fn legacy_predefined_bridge_survives_reopen() {
    let storage = Arc::new(Memory::default());
    let legacy = Network::from_spec(
        NetworkSpec::bridge("bridge", Subnet::new("172.18.0.0".parse().unwrap(), 16).unwrap()),
        7,
    );
    crate::storage::NetworkStore::insert(storage.as_ref(), &legacy)
        .await
        .unwrap();

    let containers = test_containers(storage, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();
    let reopened = containers.networks().inspect("bridge").await.unwrap();
    assert_eq!(reopened.id, legacy.id);
    assert_eq!(reopened.subnet, legacy.subnet);
    assert_eq!(reopened.created_at_ms, 7);
}

#[tokio::test]
async fn predefined_names_require_matching_drivers() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    assert!(matches!(
        containers
            .networks()
            .ensure_predefined(NetworkSpec::none("bridge"))
            .await,
        Err(Error::InvalidNetwork(_))
    ));
    assert!(matches!(
        containers
            .networks()
            .ensure_predefined(NetworkSpec::bridge(
                "none",
                Subnet::new("172.31.0.0".parse().unwrap(), 16).unwrap(),
            ))
            .await,
        Err(Error::InvalidNetwork(_))
    ));
}

#[tokio::test]
async fn generated_identity_access_follows_rootfs_writability() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("writable")).await.unwrap();
    containers.start("writable").await.unwrap();
    containers.wait("writable").await.unwrap();
    containers
        .create(spec("readonly").isolation(Isolation {
            read_only_root: true,
            ..Isolation::default()
        }))
        .await
        .unwrap();
    containers.start("readonly").await.unwrap();
    containers.wait("readonly").await.unwrap();
    containers
        .create(spec("egress").isolation(Isolation {
            network_isolated: false,
            ..Isolation::default()
        }))
        .await
        .unwrap();
    containers
        .create(
            spec("custom-resolver").resolver(
                crate::Resolver::new(
                    vec!["192.0.2.53".parse().unwrap(), "2001:db8::53".parse().unwrap()],
                    vec!["service.test".into(), "example.test".into()],
                    vec!["ndots:2".into(), "timeout:1".into()],
                )
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    containers.start("egress").await.unwrap();
    containers.wait("egress").await.unwrap();
    containers.start("custom-resolver").await.unwrap();
    containers.wait("custom-resolver").await.unwrap();

    let launches = runtime.mounts.lock().unwrap();
    for target in ["/etc/hosts", "/etc/resolv.conf", "/etc/hostname"] {
        assert!(
            launches[0]
                .iter()
                .any(|mount| mount.1 == std::path::Path::new(target) && mount.2 == Access::ReadWrite)
        );
        assert!(
            launches[1]
                .iter()
                .any(|mount| mount.1 == std::path::Path::new(target) && mount.2 == Access::ReadOnly)
        );
    }
    let resolver = launches[2]
        .iter()
        .find(|mount| mount.1 == std::path::Path::new("/etc/resolv.conf"))
        .expect("egress resolver mount");
    assert_eq!(
        std::fs::read_to_string(&resolver.0).unwrap(),
        "nameserver 127.0.0.11\noptions ndots:0\n"
    );
    let resolver = launches[3]
        .iter()
        .find(|mount| mount.1 == std::path::Path::new("/etc/resolv.conf"))
        .expect("custom resolver mount");
    assert_eq!(
        std::fs::read_to_string(&resolver.0).unwrap(),
        "nameserver 192.0.2.53\nnameserver 2001:db8::53\nsearch service.test example.test\noptions ndots:2 timeout:1\n"
    );
}

#[tokio::test]
async fn custom_bridge_identity_mounts_include_peers_and_exec_reuses_files() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(100);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers
        .networks()
        .create(NetworkSpec::bridge(
            "application",
            Subnet::new("10.91.0.0".parse().unwrap(), 24).unwrap(),
        ))
        .await
        .unwrap();
    let connected = Isolation {
        network_isolated: false,
        ..Isolation::default()
    };
    containers
        .create(spec("web").isolation(connected).hostname("web-host"))
        .await
        .unwrap();
    containers.create(spec("database").isolation(connected)).await.unwrap();
    containers.create(spec("late").isolation(connected)).await.unwrap();
    containers
        .networks()
        .connect(
            "application",
            "web",
            EndpointSpec::default().name("web").alias("frontend"),
        )
        .await
        .unwrap();
    containers
        .networks()
        .connect(
            "application",
            "database",
            EndpointSpec::default().name("database").alias("db"),
        )
        .await
        .unwrap();

    containers.start("web").await.unwrap();
    let late = containers
        .networks()
        .connect("application", "late", EndpointSpec::default())
        .await
        .unwrap();
    assert_eq!(late.address, Some("10.91.0.4".parse().unwrap()));
    let execution = containers
        .executions()
        .create("web", ExecSpec::new(Process::new("/bin/true")))
        .await
        .unwrap();
    let mut session = containers.executions().start(&execution.id).await.unwrap();
    while session.next().await.unwrap().is_some() {}
    containers.wait("web").await.unwrap();

    let launches = runtime.mounts.lock().unwrap().clone();
    assert_eq!(launches.len(), 2);
    let identity = |mounts: &Vec<(std::path::PathBuf, std::path::PathBuf, Access)>, target: &str| {
        mounts
            .iter()
            .find(|mount| mount.1 == std::path::Path::new(target))
            .cloned()
            .expect("identity mount")
    };
    let hosts = identity(&launches[0], "/etc/hosts");
    assert_eq!(hosts.2, Access::ReadWrite);
    assert_eq!(
        std::fs::read_to_string(&hosts.0).unwrap(),
        "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\nfe00::\tip6-localnet\nff00::\tip6-mcastprefix\nff02::1\tip6-allnodes\nff02::2\tip6-allrouters\n10.91.0.2\tweb frontend web-host\n10.91.0.3\tdatabase db\n10.91.0.4\tlate\n"
    );
    assert_eq!(
        std::fs::read_to_string(identity(&launches[0], "/etc/hostname").0).unwrap(),
        "web-host\n"
    );
    assert_eq!(
        std::fs::read_to_string(identity(&launches[0], "/etc/resolv.conf").0).unwrap(),
        "nameserver 127.0.0.11\noptions ndots:0\n"
    );
    for target in ["/etc/hosts", "/etc/resolv.conf", "/etc/hostname"] {
        assert_eq!(identity(&launches[0], target).0, identity(&launches[1], target).0);
    }

    let directory = hosts.0.parent().unwrap().to_owned();
    containers.remove("web").await.unwrap();
    assert!(!directory.exists());
}

#[tokio::test]
async fn published_ports_are_durable_and_only_apply_to_the_owner_process() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = service(Arc::clone(&runtime)).await;
    let publish = crate::Publication::tcp("0.0.0.0".parse().unwrap(), 18080, 8080).unwrap();
    containers.create(spec("published").publish(publish)).await.unwrap();

    containers.start("published").await.unwrap();
    containers.wait("published").await.unwrap();
    containers.start("published").await.unwrap();
    containers.wait("published").await.unwrap();

    assert_eq!(
        runtime.publishes.lock().unwrap().as_slice(),
        &[vec![publish], vec![publish]]
    );
    assert_eq!(
        containers.inspect("published").await.unwrap().spec.publish,
        vec![publish]
    );
}

#[tokio::test]
async fn automatic_host_ports_are_allocated_atomically_and_durably() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    let automatic = |guest| crate::Publication::tcp(std::net::Ipv4Addr::UNSPECIFIED, 0, guest).unwrap();
    let first = containers
        .create(spec("automatic-first").publish(automatic(80)))
        .await
        .unwrap();
    let second = containers
        .create(spec("automatic-second").publish(automatic(81)))
        .await
        .unwrap();

    assert_eq!(first.spec.publish[0].host, 49152);
    assert_eq!(second.spec.publish[0].host, 49153);
    assert_eq!(
        containers.inspect("automatic-first").await.unwrap().spec.publish[0].host,
        49152
    );
}

#[tokio::test]
async fn published_host_address_is_durable_and_reaches_the_runtime() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = service(Arc::clone(&runtime)).await;
    let publish = crate::Publication::tcp("127.0.0.1".parse().unwrap(), 18081, 8081).unwrap();
    containers
        .create(spec("loopback-published").publish(publish))
        .await
        .unwrap();

    containers.start("loopback-published").await.unwrap();
    containers.wait("loopback-published").await.unwrap();

    assert_eq!(runtime.publishes.lock().unwrap().as_slice(), &[vec![publish]]);
    assert_eq!(
        containers.inspect("loopback-published").await.unwrap().spec.publish,
        vec![publish]
    );
}

#[tokio::test]
async fn automatic_publication_attaches_the_default_bridge_at_launch() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = service(Arc::clone(&runtime)).await;
    let publish = crate::Publication::tcp("127.0.0.1".parse().unwrap(), 18082, 8082).unwrap();
    containers
        .create(
            spec("default-published")
                .isolation(Isolation {
                    network_isolated: false,
                    ..Isolation::default()
                })
                .publish(publish),
        )
        .await
        .unwrap();

    containers.start("default-published").await.unwrap();

    {
        let launches = runtime.networks.lock().unwrap();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].len(), 1);
        assert_eq!(launches[0][0].name, "bridge");
        assert_eq!(launches[0][0].driver, crate::NetworkDriver::Bridge);
    }
    let bridge = containers.networks().inspect("bridge").await.unwrap();
    assert!(
        bridge
            .endpoints
            .contains_key(&containers.inspect("default-published").await.unwrap().id)
    );
}

#[tokio::test]
async fn stopped_member_can_join_a_network_with_active_peers() {
    let mut fake = FakeRuntime::new(ExitStatus::Code(0));
    fake.delay = Duration::from_secs(1);
    let containers = service(Arc::new(fake)).await;
    let networked = |name| {
        spec(name).isolation(Isolation {
            sandbox: crate::Sandbox::Disabled,
            read_only_root: false,
            network_isolated: false,
        })
    };
    containers.create(networked("server")).await.unwrap();
    containers.create(networked("client")).await.unwrap();
    containers
        .networks()
        .create(NetworkSpec::bridge(
            "runtime",
            Subnet::new("10.55.0.0".parse().unwrap(), 24).unwrap(),
        ))
        .await
        .unwrap();
    containers
        .networks()
        .connect("runtime", "server", EndpointSpec::default())
        .await
        .unwrap();
    containers.start("server").await.unwrap();

    containers
        .networks()
        .connect("runtime", "client", EndpointSpec::default())
        .await
        .unwrap();
    assert_eq!(
        containers.networks().inspect("runtime").await.unwrap().endpoints.len(),
        2
    );
    containers.wait("server").await.unwrap();
}
