use super::support::*;

#[tokio::test]
async fn create_console_size_is_durable_and_checked() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(&docker_archive()[..], &containers.images().unwrap(), Limits::default()).unwrap();
    let socket = root.path().join("run/docker.sock");
    let daemon = TestDaemon::start(containers.clone(), &socket).await;
    let request = |tty, console_size| {
        let mut request = hl_client::model::CreateContainer {
            image: "scenario/fixture:v1".into(),
            host_config: Some(hl_client::model::HostConfig {
                console_size,
                ..Default::default()
            }),
            ..Default::default()
        };
        request.console.tty = tty;
        request
    };

    let created = daemon
        .client
        .containers()
        .create(&request(true, Some([37, 119])), Some("sized-console"))
        .await
        .unwrap();
    let expected = Some(hl_container::Size::new(37, 119).unwrap());
    assert_eq!(
        containers
            .inspect(&created.id)
            .await
            .unwrap()
            .spec
            .process
            .console
            .terminal,
        expected
    );
    for invalid in [
        request(false, Some([37, 119])),
        request(true, Some([0, 119])),
        request(true, Some([65_536, 119])),
    ] {
        assert!(matches!(
            daemon.client.containers().create(&invalid, None).await.unwrap_err(),
            hl_client::Error::Docker {
                status: http::StatusCode::BAD_REQUEST,
                ..
            }
        ));
    }
    daemon.stop().await;

    let reopened = TestDaemon::start(containers.clone(), &socket).await;
    assert_eq!(
        containers
            .inspect(&created.id)
            .await
            .unwrap()
            .spec
            .process
            .console
            .terminal,
        expected
    );
    reopened.stop().await;
}

#[tokio::test]
async fn shared_create_contract_resolves_oci_defaults_and_overrides() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(&docker_archive()[..], &containers.images().unwrap(), Limits::default()).unwrap();
    let daemon = Daemon::new(containers.clone());
    let socket = root.path().join("run/docker.sock");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        daemon
            .server(&socket)
            .serve_with_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let socket = root.path().join("run/docker.sock");
    wait_for_socket(&socket).await;

    let client = Client::unix(&socket).unwrap();
    let request = hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        cmd: Some(vec!["overridden".into()]),
        env: Some(vec!["REQUEST=yes".into()]),
        volumes: [("/anonymous".into(), serde_json::json!({}))].into_iter().collect(),
        exposed_ports: hl_client::model::ExposedPorts(
            [("8080/tcp".into(), serde_json::json!({}))].into_iter().collect(),
        ),
        host_config: Some(hl_client::model::HostConfig {
            dns: vec!["192.0.2.53".parse().unwrap(), "2001:db8::53".parse().unwrap()],
            dns_search: vec!["service.test".into()],
            dns_options: vec!["ndots:2".into(), "timeout:1".into()],
            mounts: vec![hl_client::model::DockerMount {
                kind: "volume".into(),
                source: "named-data".into(),
                target: "/data".into(),
                ..Default::default()
            }],
            port_bindings: hl_client::model::PortBindings(
                [(
                    "8080/tcp".into(),
                    Some(vec![hl_client::model::PortBinding {
                        host_ip: "0.0.0.0".into(),
                        host_port: String::new(),
                    }]),
                )]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    let created = client
        .containers()
        .create(&request, Some("created-from-image"))
        .await
        .unwrap();
    let durable = containers.inspect(&created.id).await.unwrap();
    assert_eq!(durable.spec.process.program, "/bin/echo");
    assert_eq!(durable.spec.process.args, vec!["overridden"]);
    assert_eq!(durable.spec.process.env.get_text("IMAGE").unwrap(), "yes");
    assert_eq!(durable.spec.process.env.get_text("REQUEST").unwrap(), "yes");
    assert_eq!(durable.spec.process.working_dir.to_str().unwrap(), "/work");
    assert_eq!(durable.spec.publish.len(), 1);
    assert_eq!(durable.spec.publish[0].host, 49152);
    assert_eq!(durable.spec.publish[0].port.guest, 8080);
    assert_eq!(
        durable.spec.resolver.nameservers(),
        [
            "192.0.2.53".parse::<std::net::IpAddr>().unwrap(),
            "2001:db8::53".parse::<std::net::IpAddr>().unwrap(),
        ]
    );
    assert_eq!(durable.spec.resolver.search(), ["service.test"]);
    assert_eq!(durable.spec.resolver.options(), ["ndots:2", "timeout:1"]);
    let inspected = client.containers().inspect(&created.id).await.unwrap();
    assert_eq!(
        inspected.host_config.dns,
        [
            "192.0.2.53".parse::<std::net::IpAddr>().unwrap(),
            "2001:db8::53".parse::<std::net::IpAddr>().unwrap(),
        ]
    );
    assert_eq!(inspected.host_config.dns_search, ["service.test"]);
    assert_eq!(inspected.host_config.dns_options, ["ndots:2", "timeout:1"]);
    assert_eq!(
        inspected.network_settings.ports["8080/tcp"].as_ref().unwrap()[0].host_port,
        "49152"
    );
    assert_eq!(
        client.containers().list(true).await.unwrap()[0].ports[0].public_port,
        Some(49152)
    );
    let udp = hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        host_config: Some(hl_client::model::HostConfig {
            port_bindings: hl_client::model::PortBindings(
                [("53/udp".into(), Some(vec![hl_client::model::PortBinding::default()]))]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(matches!(
        client.containers().create(&udp, None).await.unwrap_err(),
        hl_client::Error::Docker {
            status: http::StatusCode::BAD_REQUEST,
            ref message,
        } if message.contains("only tcp")
    ));
    let loopback = hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        host_config: Some(hl_client::model::HostConfig {
            port_bindings: hl_client::model::PortBindings(
                [(
                    "8080/tcp".into(),
                    Some(vec![hl_client::model::PortBinding {
                        host_ip: "127.0.0.1".into(),
                        host_port: "18080".into(),
                    }]),
                )]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    let loopback = client
        .containers()
        .create(&loopback, Some("loopback-published"))
        .await
        .unwrap();
    let loopback = client.containers().inspect(&loopback.id).await.unwrap();
    let binding = &loopback.network_settings.ports["8080/tcp"].as_ref().unwrap()[0];
    assert_eq!(binding.host_ip, "127.0.0.1");
    assert_eq!(binding.host_port, "18080");
    let host = hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        host_config: Some(hl_client::model::HostConfig {
            network_mode: "host".into(),
            port_bindings: hl_client::model::PortBindings(
                [(
                    "8080/tcp".into(),
                    Some(vec![hl_client::model::PortBinding {
                        host_ip: "127.0.0.1".into(),
                        host_port: "28080".into(),
                    }]),
                )]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    let host = client.containers().create(&host, Some("host-networked")).await.unwrap();
    assert_eq!(host.warnings.len(), 1);
    assert!(host.warnings[0].contains("Published ports are discarded"));
    let durable_host = containers.inspect(&host.id).await.unwrap();
    assert_eq!(durable_host.spec.network_mode, hl_container::NetworkMode::Host);
    assert!(durable_host.spec.publish.is_empty());
    assert!(
        containers
            .networks()
            .list()
            .await
            .unwrap()
            .iter()
            .all(|network| !network.endpoints.contains_key(&durable_host.id))
    );
    let host_inspect = client.containers().inspect(&host.id).await.unwrap();
    assert_eq!(host_inspect.host_config.network_mode, "host");
    assert!(host_inspect.network_settings.networks.is_empty());
    assert!(host_inspect.network_settings.ports.is_empty());
    let anonymous = assert_created_mounts(&containers, &client, &created.id, &durable).await;

    let invalid = hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        env: Some(vec!["MISSING_SEPARATOR".into()]),
        ..Default::default()
    };
    assert!(matches!(
        client.containers().create(&invalid, None).await.unwrap_err(),
        hl_client::Error::Docker {
            status: http::StatusCode::BAD_REQUEST,
            ref message,
        } if message.contains("expected NAME=VALUE")
    ));
    let unsupported_bind = hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        host_config: Some(hl_client::model::HostConfig {
            mounts: vec![hl_client::model::DockerMount {
                kind: "bind".into(),
                source: root.path().to_string_lossy().into_owned(),
                target: "/shared".into(),
                bind_options: Some(hl_client::model::BindOptions {
                    propagation: "rshared".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(matches!(
        client
            .containers()
            .create(&unsupported_bind, Some("unsupported-bind"))
            .await,
        Err(hl_client::Error::Docker {
            status: http::StatusCode::NOT_IMPLEMENTED,
            ..
        })
    ));
    assert!(containers.inspect("unsupported-bind").await.is_err());
    client.containers().remove(&created.id, false, true).await.unwrap();
    assert!(matches!(
        containers.volumes().inspect(&anonymous).await,
        Err(hl_container::Error::VolumeNotFound(_))
    ));
    assert!(containers.volumes().inspect("named-data").await.is_ok());

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

async fn assert_created_mounts(
    containers: &Containers,
    client: &Client,
    id: &str,
    durable: &hl_container::Container,
) -> String {
    let inspected = client.containers().inspect(id).await.unwrap();
    assert_eq!(inspected.metadata.image, "docker.io/scenario/fixture:v1");
    assert_eq!(inspected.metadata.mounts.len(), 2);
    assert!(inspected.metadata.mounts.iter().any(|mount| {
        mount.kind == "volume" && mount.name == "named-data" && mount.destination == "/data" && mount.read_write
    }));
    let anonymous = durable
        .spec
        .mounts
        .iter()
        .find_map(|mount| match &mount.source {
            hl_container::MountSource::Anonymous(name) => Some(name.clone()),
            _ => None,
        })
        .expect("Config.Volumes did not create an anonymous semantic mount");
    let volume = containers.volumes().inspect(&anonymous).await.unwrap();
    assert_eq!(std::fs::read(volume.path.join("seed")).unwrap(), b"from-image");
    let payload = docker_tar(&[("value", b"mounted")]);
    containers
        .filesystem(id)
        .await
        .unwrap()
        .extract("/anonymous", &payload[..], hl_container::Limits::default())
        .unwrap();
    assert_eq!(std::fs::read(volume.path.join("value")).unwrap(), b"mounted");
    anonymous
}
