use super::support::*;

#[tokio::test]
async fn compatibility_metadata_surfaces_are_typed_and_truthful() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let socket = root.path().join("run/docker.sock");
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
    assert!(client.system().plugins().await.unwrap().is_empty());
    let authentication = client
        .system()
        .authenticate(&hl_client::model::Credentials {
            username: "guest".into(),
            password: "secret".into(),
            email: "guest@example.test".into(),
            serveraddress: "registry.example.test".into(),
            identity_token: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(authentication.status, "Login Succeeded");
    assert!(authentication.identity_token.is_empty());
    assert!(client.images().search("alpine", Some(5)).await.unwrap().is_empty());

    let distribution = raw_http(
        &socket,
        b"GET /v1.43/distribution/alpine/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(distribution.starts_with("HTTP/1.1 404"));
    assert!(distribution.contains("No such distribution: alpine"));

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn container_changes_compare_owned_rootfs_with_immutable_image_baseline() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(&docker_archive()[..], &containers.images().unwrap(), Limits::default()).unwrap();
    let socket = root.path().join("run/docker.sock");
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
    let created = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/fixture:v1".into(),
                ..Default::default()
            },
            Some("changes-source"),
        )
        .await
        .unwrap();
    assert!(client.containers().changes(&created.id).await.unwrap().is_empty());

    let container = containers.inspect(&created.id).await.unwrap();
    let hl_container::Rootfs::Image(reference) = container.spec.rootfs else {
        panic!("Docker image creation must own an image rootfs");
    };
    let images = containers.images().unwrap();
    let view = images.roots().open(&reference).unwrap();
    if reference.overlay().is_some() {
        std::fs::create_dir_all(view.path().join("etc")).unwrap();
    }
    std::fs::write(view.path().join("etc/release"), b"modified\n").unwrap();
    std::fs::write(view.path().join("added"), b"new\n").unwrap();
    if reference.overlay().is_some() {
        std::fs::create_dir_all(view.path().join("anonymous")).unwrap();
        std::fs::write(view.path().join("anonymous/.wh.seed"), b"").unwrap();
    } else {
        std::fs::remove_file(view.path().join("anonymous/seed")).unwrap();
    }

    assert_eq!(
        client.containers().changes(&created.id).await.unwrap(),
        vec![
            hl_client::model::Change {
                path: "/added".into(),
                kind: hl_client::model::ChangeKind::Added,
            },
            hl_client::model::Change {
                path: "/anonymous/seed".into(),
                kind: hl_client::model::ChangeKind::Deleted,
            },
            hl_client::model::Change {
                path: "/etc/release".into(),
                kind: hl_client::model::ChangeKind::Modified,
            },
        ]
    );

    client.containers().remove(&created.id, false, false).await.unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
