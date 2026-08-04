use hl_container::{
    Config, ContainerSpec, Containers, EndpointSpec, Error, NetworkDriver, NetworkSpec, Process,
    Subnet,
};
use std::net::Ipv4Addr;

fn container(root: &std::path::Path, name: &str) -> ContainerSpec {
    let rootfs = root.join(format!("rootfs-{name}"));
    std::fs::create_dir_all(&rootfs).unwrap();
    ContainerSpec::from_directory(rootfs, Process::new("/bin/true")).name(name)
}

#[tokio::test]
async fn deterministic_ipam_connections_and_cleanup_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let config = Config::new(root.path());
    let containers = Containers::builder(config.clone()).build().await.unwrap();
    containers
        .create(container(root.path(), "one"))
        .await
        .unwrap();
    containers
        .create(container(root.path(), "two"))
        .await
        .unwrap();
    let subnet = Subnet::new(Ipv4Addr::new(10, 44, 0, 0), 29).unwrap();
    let network = containers
        .networks()
        .create(NetworkSpec::bridge("private", subnet).label("scope", "test"))
        .await
        .unwrap();
    assert_eq!(network.driver, NetworkDriver::Bridge);

    let networks = containers.networks();
    let first = {
        let networks = networks.clone();
        tokio::spawn(async move {
            networks
                .connect("private", "one", EndpointSpec::default().alias("one.local"))
                .await
        })
    };
    let second = {
        let networks = networks.clone();
        tokio::spawn(async move {
            networks
                .connect("private", "two", EndpointSpec::default().alias("two.local"))
                .await
        })
    };
    let mut addresses = vec![
        first.await.unwrap().unwrap().address.unwrap(),
        second.await.unwrap().unwrap().address.unwrap(),
    ];
    addresses.sort();
    assert_eq!(
        addresses,
        vec![Ipv4Addr::new(10, 44, 0, 2), Ipv4Addr::new(10, 44, 0, 3)]
    );
    assert!(matches!(
        networks.remove("private").await,
        Err(Error::NetworkInUse(_))
    ));
    drop(containers);

    let reopened = Containers::builder(config).build().await.unwrap();
    let persisted = reopened
        .networks()
        .inspect(&network.id.to_string())
        .await
        .unwrap();
    assert_eq!(persisted.endpoints.len(), 2);
    assert!(persisted
        .endpoints
        .values()
        .all(|endpoint| !endpoint.name.is_empty()));
    assert_eq!(
        persisted
            .endpoints
            .values()
            .map(|endpoint| endpoint.aliases[0].as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["one.local", "two.local"])
    );
    assert_eq!(
        reopened
            .networks()
            .inspect(&network.id.as_str()[..12])
            .await
            .unwrap(),
        persisted
    );
    reopened
        .networks()
        .disconnect("private", "one")
        .await
        .unwrap();
    reopened.remove("two").await.unwrap();
    assert!(reopened
        .networks()
        .inspect("private")
        .await
        .unwrap()
        .endpoints
        .is_empty());
    assert_eq!(reopened.networks().prune().await.unwrap().len(), 1);
}

#[tokio::test]
async fn forced_removal_drops_durable_endpoint_attachments() {
    let root = tempfile::tempdir().unwrap();
    let config = Config::new(root.path());
    let containers = Containers::builder(config.clone()).build().await.unwrap();
    containers
        .create(container(root.path(), "attached"))
        .await
        .unwrap();
    let networks = containers.networks();
    networks.create(NetworkSpec::bridge_auto("temporary")).await.unwrap();
    networks
        .connect("temporary", "attached", EndpointSpec::default())
        .await
        .unwrap();

    assert!(matches!(
        networks.remove("temporary").await,
        Err(Error::NetworkInUse(_))
    ));
    let removed = networks.force_remove("temporary").await.unwrap();
    assert_eq!(removed.endpoints.len(), 1);
    assert!(matches!(
        networks.inspect("temporary").await,
        Err(Error::NetworkNotFound(_))
    ));

    drop(containers);
    let reopened = Containers::builder(config).build().await.unwrap();
    assert!(reopened
        .networks()
        .list()
        .await
        .unwrap()
        .iter()
        .all(|network| network.name != "temporary"));
}

#[tokio::test]
async fn network_validation_rejects_overlap_conflicts_and_duplicate_addresses() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()))
        .build()
        .await
        .unwrap();
    containers
        .create(container(root.path(), "one"))
        .await
        .unwrap();
    containers
        .create(container(root.path(), "two"))
        .await
        .unwrap();
    let networks = containers.networks();
    let subnet = Subnet::new(Ipv4Addr::new(172, 30, 0, 0), 24).unwrap();
    let original = networks
        .create(NetworkSpec::bridge("main", subnet))
        .await
        .unwrap();
    assert_eq!(
        networks
            .create(NetworkSpec::bridge("main", subnet))
            .await
            .unwrap(),
        original
    );
    assert!(matches!(
        networks
            .create(NetworkSpec::bridge(
                "overlap",
                Subnet::new(Ipv4Addr::new(172, 30, 0, 128), 25).unwrap()
            ))
            .await,
        Err(Error::InvalidNetwork(_))
    ));
    let address = Ipv4Addr::new(172, 30, 0, 50);
    networks
        .connect(
            "main",
            "one",
            EndpointSpec::default().address(address).alias("primary"),
        )
        .await
        .unwrap();
    networks.create(NetworkSpec::bridge_auto("second")).await.unwrap();
    networks
        .connect("second", "one", EndpointSpec::default())
        .await
        .unwrap();
    assert!(networks
        .inspect("second")
        .await
        .unwrap()
        .endpoints
        .contains_key(&containers.inspect("one").await.unwrap().id));
    assert!(matches!(
        networks
            .connect("main", "two", EndpointSpec::default().address(address))
            .await,
        Err(Error::InvalidNetwork(_))
    ));
    assert!(matches!(
        networks
            .connect("main", "one", EndpointSpec::default())
            .await,
        Err(Error::AlreadyConnected { .. })
    ));
    for name in ["", "../escape", "/absolute", "bad name"] {
        assert!(matches!(
            networks.create(NetworkSpec::none(name)).await,
            Err(Error::InvalidNetwork(_))
        ));
    }
}

#[tokio::test]
async fn multiple_connections_validate_first_and_commit_together() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()))
        .build()
        .await
        .unwrap();
    let container = containers
        .create(container(root.path(), "multi"))
        .await
        .unwrap();
    let networks = containers.networks();
    networks
        .create(NetworkSpec::bridge_auto("frontend"))
        .await
        .unwrap();
    networks
        .create(NetworkSpec::bridge_auto("backend"))
        .await
        .unwrap();

    assert!(networks
        .connect_many(
            "multi",
            [
                ("frontend".into(), EndpointSpec::default().alias("web")),
                ("missing".into(), EndpointSpec::default().alias("db")),
            ],
        )
        .await
        .is_err());
    assert!(networks
        .inspect("frontend")
        .await
        .unwrap()
        .endpoints
        .is_empty());

    networks
        .connect_many(
            "multi",
            [
                ("frontend".into(), EndpointSpec::default().alias("web")),
                ("backend".into(), EndpointSpec::default().alias("db")),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        networks.inspect("frontend").await.unwrap().endpoints[&container.id].aliases,
        ["web"]
    );
    assert_eq!(
        networks.inspect("backend").await.unwrap().endpoints[&container.id].aliases,
        ["db"]
    );
}

#[tokio::test]
async fn none_network_has_no_ipam() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()))
        .build()
        .await
        .unwrap();
    containers
        .create(container(root.path(), "isolated"))
        .await
        .unwrap();
    let endpoint = containers
        .networks()
        .connect("none", "isolated", EndpointSpec::default())
        .await
        .unwrap();
    assert_eq!(endpoint.address, None);
    assert!(matches!(
        containers
            .networks()
            .connect(
                "none",
                "isolated",
                EndpointSpec::default().address(Ipv4Addr::LOCALHOST)
            )
            .await,
        Err(Error::AlreadyConnected { .. })
    ));
}

#[tokio::test]
async fn automatic_bridge_subnets_are_atomic_deterministic_and_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()))
        .build()
        .await
        .unwrap();
    let networks = containers.networks();

    let first = networks
        .create(NetworkSpec::bridge_auto("first"))
        .await
        .unwrap();
    assert_eq!(
        first.subnet,
        Some(Subnet::new(Ipv4Addr::new(172, 18, 0, 0), 16).unwrap())
    );
    assert_eq!(
        networks
            .create(NetworkSpec::bridge_auto("first"))
            .await
            .unwrap(),
        first
    );
    let second = networks
        .create(NetworkSpec::bridge_auto("second"))
        .await
        .unwrap();
    assert_eq!(
        second.subnet,
        Some(Subnet::new(Ipv4Addr::new(172, 19, 0, 0), 16).unwrap())
    );
}

#[tokio::test]
async fn null_driver_is_singleton_and_network_policy_survives_reopen() {
    let root = tempfile::tempdir().unwrap();
    let config = Config::new(root.path());
    let containers = Containers::builder(config.clone()).build().await.unwrap();
    assert!(matches!(
        containers.networks().create(NetworkSpec::none("airgap")).await,
        Err(Error::NetworkConflict(name)) if name == "airgap"
    ));
    assert!(matches!(
        containers.networks().create(NetworkSpec::none("none")).await,
        Err(Error::NetworkConflict(name)) if name == "none"
    ));
    let internal = containers
        .networks()
        .create(
            NetworkSpec::bridge_auto("internal")
                .internal(true)
                .attachable(true),
        )
        .await
        .unwrap();
    assert!(internal.internal);
    assert!(internal.attachable);

    drop(containers);
    let reopened = Containers::builder(config).build().await.unwrap();
    assert!(reopened.networks().inspect("internal").await.unwrap().internal);
    assert!(reopened.networks().inspect("internal").await.unwrap().attachable);
    assert!(!reopened.networks().inspect("none").await.unwrap().internal);
    assert!(!reopened.networks().inspect("none").await.unwrap().attachable);
}

#[tokio::test]
async fn predefined_networks_survive_headless_remove_and_prune() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()))
        .build()
        .await
        .unwrap();
    let networks = containers.networks();
    networks.create(NetworkSpec::bridge_auto("unused")).await.unwrap();

    assert!(matches!(
        networks.remove("none").await,
        Err(Error::InvalidNetwork(message)) if message.contains("predefined")
    ));
    assert!(matches!(
        networks.force_remove("bridge").await,
        Err(Error::InvalidNetwork(message)) if message.contains("predefined")
    ));
    assert_eq!(
        networks
            .prune()
            .await
            .unwrap()
            .into_iter()
            .map(|network| network.name)
            .collect::<Vec<_>>(),
        ["unused"]
    );
    assert!(networks.inspect("none").await.unwrap().predefined());
    assert!(networks.inspect("bridge").await.unwrap().predefined());
}

#[tokio::test]
async fn host_mode_never_allocates_a_virtual_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()))
        .build()
        .await
        .unwrap();
    let spec = container(root.path(), "host")
        .isolation(hl_container::Isolation {
            network_isolated: false,
            ..hl_container::Isolation::default()
        })
        .network_mode(hl_container::NetworkMode::Host);
    containers.create(spec).await.unwrap();
    containers
        .networks()
        .create(NetworkSpec::bridge_auto("virtual"))
        .await
        .unwrap();
    assert!(matches!(
        containers
            .networks()
            .connect("virtual", "host", EndpointSpec::default())
            .await,
        Err(Error::InvalidNetwork(message)) if message.contains("host network mode")
    ));
    assert!(containers
        .networks()
        .inspect("virtual")
        .await
        .unwrap()
        .endpoints
        .is_empty());
}
