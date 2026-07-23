use super::support::*;

#[tokio::test]
async fn container_inspect_size_accounts_executed_rootfs_writes() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &runnable_archive()[..],
        &containers.images().unwrap(),
        Limits::default(),
    )
    .unwrap();
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
    let created = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/runnable:v1".into(),
                entrypoint: Some(vec!["/bin/sh".into()]),
                cmd: Some(vec!["-c".into(), "printf 1234567 > /delta".into()]),
                ..Default::default()
            },
            Some("inspect-size"),
        )
        .await
        .unwrap();
    let ordinary = client.containers().inspect(&created.id).await.unwrap();
    assert_eq!(ordinary.size_rw, None);
    assert_eq!(ordinary.size_root_fs, None);
    let ordinary_wire = raw_http(
        &socket,
        format!(
            "GET /v1.43/containers/{}/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            created.id
        )
        .as_bytes(),
    )
    .await;
    assert!(!ordinary_wire.contains("\"SizeRw\""));
    assert!(!ordinary_wire.contains("\"SizeRootFs\""));

    client.containers().start(&created.id).await.unwrap();
    assert_eq!(
        client
            .containers()
            .wait(&created.id)
            .await
            .unwrap()
            .status_code,
        0
    );
    let sized = client
        .containers()
        .inspect_with_size(&created.id, true)
        .await
        .unwrap();
    assert!(sized.size_rw.is_some_and(|bytes| bytes >= 7));
    assert!(sized.size_root_fs > sized.size_rw);
    let sized_wire = raw_http(
        &socket,
        format!(
            "GET /v1.43/containers/{}/json?size=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            created.id
        )
        .as_bytes(),
    )
    .await;
    assert!(sized_wire.contains("\"SizeRw\":"));
    assert!(sized_wire.contains("\"SizeRootFs\":"));
    let invalid = raw_http(
        &socket,
        format!(
            "GET /v1.43/containers/{}/json?size=maybe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            created.id
        )
        .as_bytes(),
    )
    .await;
    assert!(invalid.starts_with("HTTP/1.1 400"));

    client
        .containers()
        .remove(&created.id, false, false)
        .await
        .unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn image_list_shared_size_accounts_executed_child_layers() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &runnable_archive()[..],
        &containers.images().unwrap(),
        Limits::default(),
    )
    .unwrap();
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
    let created = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/runnable:v1".into(),
                entrypoint: Some(vec!["/bin/sh".into()]),
                cmd: Some(vec!["-c".into(), "printf child > /child".into()]),
                ..Default::default()
            },
            Some("shared-size-source"),
        )
        .await
        .unwrap();
    client.containers().start(&created.id).await.unwrap();
    assert_eq!(
        client
            .containers()
            .wait(&created.id)
            .await
            .unwrap()
            .status_code,
        0
    );
    client
        .images()
        .commit(&created.id, "scenario/shared-child", Some("v1"), false)
        .await
        .unwrap();
    client
        .images()
        .tag(
            "scenario/runnable:v1",
            "scenario/runnable-alias",
            Some("v1"),
        )
        .await
        .unwrap();

    let ordinary = client.images().list().await.unwrap();
    assert_eq!(ordinary.len(), 2);
    assert!(ordinary.iter().any(|image| image.repo_tags.len() == 2));
    assert!(ordinary.iter().all(|image| image.shared_size == -1));
    let accounted = client.images().list_with_shared_size(true).await.unwrap();
    assert_eq!(accounted.len(), 2);
    assert!(accounted.iter().all(|image| image.shared_size > 0));
    assert!(accounted
        .iter()
        .all(|image| image.shared_size <= image.size));

    client
        .containers()
        .remove(&created.id, false, false)
        .await
        .unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn explicit_rw_local_bind_volume_persists_executed_writes() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &runnable_archive()[..],
        &containers.images().unwrap(),
        Limits::default(),
    )
    .unwrap();
    let device = root.path().join("rw-device");
    std::fs::create_dir(&device).unwrap();
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
    client
        .volumes()
        .create(&VolumeCreate {
            name: "rw-local".into(),
            driver: "local".into(),
            driver_opts: BTreeMap::from([
                ("type".into(), "none".into()),
                ("o".into(), "bind,rw".into()),
                ("device".into(), device.to_string_lossy().into_owned()),
            ]),
            ..Default::default()
        })
        .await
        .unwrap();
    let created = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/runnable:v1".into(),
                entrypoint: Some(vec!["/bin/sh".into()]),
                cmd: Some(vec!["-c".into(), "printf mounted > /data/value".into()]),
                host_config: Some(hl_client::model::HostConfig {
                    binds: vec!["rw-local:/data:rw".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some("rw-local-writer"),
        )
        .await
        .unwrap();
    client.containers().start(&created.id).await.unwrap();
    assert_eq!(
        client
            .containers()
            .wait(&created.id)
            .await
            .unwrap()
            .status_code,
        0
    );
    assert_eq!(std::fs::read(device.join("value")).unwrap(), b"mounted");
    assert!(
        client
            .containers()
            .inspect(&created.id)
            .await
            .unwrap()
            .metadata
            .mounts[0]
            .read_write
    );

    client
        .containers()
        .remove(&created.id, false, false)
        .await
        .unwrap();
    client.volumes().remove("rw-local", false).await.unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn legacy_host_config_tmpfs_executes_and_is_reclaimed() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &runnable_archive()[..],
        &containers.images().unwrap(),
        Limits::default(),
    )
    .unwrap();
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
    let created = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/runnable:v1".into(),
                entrypoint: Some(vec!["/bin/sh".into()]),
                cmd: Some(vec![
                    "-c".into(),
                    "printf ephemeral > /scratch/value && cat /scratch/value".into(),
                ]),
                host_config: Some(hl_client::model::HostConfig {
                    tmpfs: BTreeMap::from([("/scratch".into(), "rw".into())]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some("legacy-tmpfs"),
        )
        .await
        .unwrap();
    client.containers().start(&created.id).await.unwrap();
    assert_eq!(
        client
            .containers()
            .wait(&created.id)
            .await
            .unwrap()
            .status_code,
        0
    );
    assert_eq!(
        client
            .containers()
            .logs(&created.id, true, true)
            .await
            .unwrap()
            .stdout,
        b"ephemeral"
    );
    let inspect = client.containers().inspect(&created.id).await.unwrap();
    assert_eq!(inspect.metadata.mounts[0].kind, "tmpfs");
    assert_eq!(inspect.metadata.mounts[0].destination, "/scratch");
    let mountpoint = std::path::PathBuf::from(&inspect.metadata.mounts[0].source);
    assert!(mountpoint.join("value").exists());

    client
        .containers()
        .remove(&created.id, false, false)
        .await
        .unwrap();
    assert!(!mountpoint.exists());
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
