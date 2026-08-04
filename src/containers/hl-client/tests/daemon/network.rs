use super::support::*;

#[tokio::test]
async fn network_client_and_server_share_headless_topology() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let container = containers
        .create(ContainerSpec::from_directory(root.path(), Process::new("/bin/true")).name("job"))
        .await
        .unwrap();
    let socket = root.path().join("network.sock");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        Daemon::new(containers)
            .server(&socket)
            .serve_with_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    wait_for_socket(&root.path().join("network.sock")).await;

    let client = Client::unix(root.path().join("network.sock")).unwrap();
    let created = client
        .networks()
        .create(&NetworkCreate {
            name: "isolated".into(),
            driver: "none".into(),
            internal: true,
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .networks()
        .connect(
            &created.id,
            &NetworkConnect {
                container: container.id.to_string(),
                endpoint_config: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let inspected = client.networks().inspect("isolated").await.unwrap();
    assert_eq!(inspected.id, created.id);
    assert_eq!(inspected.containers.len(), 1);
    assert_eq!(inspected.containers.values().next().unwrap().name, "job");

    let error = client.networks().remove("isolated", false).await.unwrap_err();
    assert!(matches!(
        error,
        hl_client::Error::Docker {
            status: http::StatusCode::FORBIDDEN,
            ..
        }
    ));
    client
        .networks()
        .disconnect(
            "isolated",
            &NetworkDisconnect {
                container: "job".into(),
                force: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(client.networks().prune().await.unwrap().networks_deleted, ["isolated"]);
    client
        .networks()
        .create(&NetworkCreate {
            name: "none".into(),
            driver: "none".into(),
            internal: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(matches!(
        client.networks().remove("none", true).await,
        Err(hl_client::Error::Docker {
            status: http::StatusCode::FORBIDDEN,
            ..
        })
    ));
    assert!(client.networks().prune().await.unwrap().networks_deleted.is_empty());

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn forced_network_removal_uses_the_docker_delete_contract() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let socket = root.path().join("network-force.sock");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn({
        let socket = socket.clone();
        async move {
            Daemon::new(containers)
                .server(&socket)
                .serve_with_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        }
    });
    wait_for_socket(&socket).await;

    let client = Client::unix(&socket).unwrap();
    client
        .networks()
        .create(&NetworkCreate {
            name: "force-candidate".into(),
            driver: "none".into(),
            internal: true,
            ..Default::default()
        })
        .await
        .unwrap();
    client.networks().remove("force-candidate", true).await.unwrap();
    assert!(matches!(
        client.networks().inspect("force-candidate").await,
        Err(hl_client::Error::Docker {
            status: http::StatusCode::NOT_FOUND,
            ..
        })
    ));

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

fn multi_network_request(second: &str) -> hl_client::model::CreateContainer {
    let endpoint = |alias: &str| hl_client::model::EndpointConfig {
        aliases: vec![alias.into()],
        ..Default::default()
    };
    hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        host_config: Some(hl_client::model::HostConfig {
            network_mode: "frontend".into(),
            ..Default::default()
        }),
        networking_config: Some(hl_client::model::NetworkingConfig {
            endpoints_config: hl_client::model::EndpointsConfig(
                [
                    ("frontend".into(), endpoint("web")),
                    (second.into(), endpoint("database")),
                ]
                .into_iter()
                .collect(),
            ),
        }),
        ..Default::default()
    }
}

fn volume_subpath_request(subpath: &str) -> hl_client::model::CreateContainer {
    hl_client::model::CreateContainer {
        image: "scenario/fixture:v1".into(),
        host_config: Some(hl_client::model::HostConfig {
            mounts: vec![hl_client::model::DockerMount {
                kind: "volume".into(),
                source: "subpath-data".into(),
                target: "/data".into(),
                read_only: true,
                volume_options: Some(hl_client::model::VolumeOptions {
                    subpath: Some(subpath.into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn create_attaches_multiple_networks_with_aliases_atomically() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(&docker_archive()[..], &containers.images().unwrap(), Limits::default()).unwrap();
    for (name, address) in [("frontend", "10.81.0.0"), ("backend", "10.82.0.0")] {
        containers
            .networks()
            .create(hl_container::NetworkSpec::bridge(
                name,
                hl_container::Subnet::new(address.parse().unwrap(), 24).unwrap(),
            ))
            .await
            .unwrap();
    }
    let socket = root.path().join("network-create.sock");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn({
        let socket = socket.clone();
        let containers = containers.clone();
        async move {
            Daemon::new(containers)
                .server(&socket)
                .serve_with_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        }
    });
    wait_for_socket(&socket).await;
    let client = Client::unix(&socket).unwrap();

    assert!(
        client
            .containers()
            .create(&multi_network_request("missing"), Some("invalid-multi"))
            .await
            .is_err()
    );
    assert!(containers.inspect("invalid-multi").await.is_err());
    assert!(
        containers
            .networks()
            .inspect("frontend")
            .await
            .unwrap()
            .endpoints
            .is_empty()
    );

    let created = client
        .containers()
        .create(&multi_network_request("backend"), Some("valid-multi"))
        .await
        .unwrap();
    assert_eq!(
        client.networks().inspect("frontend").await.unwrap().containers[&created.id].name,
        "valid-multi"
    );
    assert_eq!(
        containers.networks().inspect("backend").await.unwrap().endpoints
            [&containers.inspect(&created.id).await.unwrap().id]
            .aliases,
        ["database"]
    );

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn volume_subpath_create_preserves_access_and_rejects_unsafe_resolution() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(&docker_archive()[..], &containers.images().unwrap(), Limits::default()).unwrap();
    let volume = containers
        .volumes()
        .create(hl_container::VolumeSpec::new("subpath-data"))
        .await
        .unwrap();
    std::fs::create_dir_all(volume.path().join("tenant/alpha")).unwrap();
    std::fs::write(volume.path().join("tenant/alpha/value"), b"safe").unwrap();
    std::fs::create_dir(root.path().join("outside-subpath")).unwrap();
    std::os::unix::fs::symlink(root.path().join("outside-subpath"), volume.path().join("escape")).unwrap();
    let socket = root.path().join("subpath.sock");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn({
        let socket = socket.clone();
        let containers = containers.clone();
        async move {
            Daemon::new(containers)
                .server(&socket)
                .serve_with_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        }
    });
    wait_for_socket(&socket).await;
    let client = Client::unix(&socket).unwrap();

    let valid = client
        .containers()
        .create(&volume_subpath_request("tenant/alpha"), Some("subpath-valid"))
        .await
        .unwrap();
    assert_eq!(
        containers
            .filesystem(&valid.id)
            .await
            .unwrap()
            .stat("/data/value")
            .unwrap()
            .size,
        4
    );
    assert_eq!(
        containers.inspect(&valid.id).await.unwrap().spec.mounts[0].access,
        hl_container::Access::ReadOnly
    );
    for (name, path) in [("subpath-missing-api", "missing"), ("subpath-escape-api", "escape")] {
        let created = client
            .containers()
            .create(&volume_subpath_request(path), Some(name))
            .await
            .unwrap();
        assert!(client.containers().start(&created.id).await.is_err());
    }
    assert!(
        client
            .containers()
            .create(&volume_subpath_request("../outside"), Some("subpath-parent-api"))
            .await
            .is_err()
    );

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
