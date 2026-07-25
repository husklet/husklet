use super::support::*;

#[tokio::test]
async fn volume_crud_is_shared_with_headless_ownership_and_protects_references() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let socket = root.path().join("run/docker.sock");
    let daemon = Daemon::new(containers.clone());
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
    let request = VolumeCreate {
        name: "shared-data".into(),
        labels: [("purpose".into(), "compatibility".into())]
            .into_iter()
            .collect(),
        ..VolumeCreate::default()
    };
    let volume = client.volumes().create(&request).await.unwrap();
    assert_eq!(volume.name, "shared-data");
    assert_eq!(
        client.volumes().create(&request).await.unwrap().mountpoint,
        volume.mountpoint
    );
    std::fs::write(
        std::path::Path::new(&volume.mountpoint).join("value"),
        b"durable",
    )
    .unwrap();
    assert_eq!(
        client.volumes().inspect("shared-data").await.unwrap(),
        volume
    );

    containers
        .create(
            ContainerSpec::from_directory("/rootfs", Process::new("/bin/true"))
                .name("volume-owner")
                .mount(Mount::volume(
                    &volume.name,
                    "/data",
                    hl_container::Access::ReadWrite,
                )),
        )
        .await
        .unwrap();
    let usage = client.system().disk_usage().await.unwrap();
    assert_eq!(usage.volumes.len(), 1);
    assert_eq!(usage.volumes[0].name, "shared-data");
    assert_eq!(usage.volumes[0].usage_data.size, 7);
    assert_eq!(usage.volumes[0].usage_data.ref_count, 1);
    let error = client
        .volumes()
        .remove("shared-data", true)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        hl_client::Error::Docker {
            status: http::StatusCode::CONFLICT,
            ..
        }
    ));
    containers.remove("volume-owner").await.unwrap();
    client.volumes().remove("shared-data", false).await.unwrap();
    assert!(!std::path::Path::new(&volume.mountpoint).exists());

    let anonymous = client
        .volumes()
        .create(&VolumeCreate::default())
        .await
        .unwrap();
    assert!(!anonymous.name.is_empty());
    assert_eq!(client.volumes().list().await.unwrap().volumes.len(), 1);
    assert_eq!(
        client.volumes().prune().await.unwrap().volumes_deleted,
        vec![anonymous.name]
    );
    assert!(client.volumes().list().await.unwrap().volumes.is_empty());

    verify_granted_volume(&client, &root).await;

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

async fn verify_granted_volume(client: &Client, root: &TempDir) {
    let device = root.path().join("granted-volume");
    std::fs::create_dir(&device).unwrap();
    std::fs::write(device.join("value"), b"host-owned").unwrap();
    let options: std::collections::BTreeMap<String, String> = [
        ("type".into(), "none".into()),
        ("o".into(), "bind,ro".into()),
        ("device".into(), device.to_string_lossy().into_owned()),
    ]
    .into_iter()
    .collect();
    let granted = client
        .volumes()
        .create(&VolumeCreate {
            name: "granted-volume".into(),
            driver: "local".into(),
            driver_opts: options.clone(),
            ..VolumeCreate::default()
        })
        .await
        .unwrap();
    assert_eq!(
        (granted.driver.as_str(), granted.scope.as_str()),
        ("local", "local")
    );
    assert_eq!(granted.options, options);
    assert_eq!(
        granted.mountpoint,
        std::fs::canonicalize(&device).unwrap().to_string_lossy()
    );
    assert_eq!(
        client.volumes().inspect("granted-volume").await.unwrap(),
        granted
    );
    client
        .volumes()
        .remove("granted-volume", false)
        .await
        .unwrap();
    assert_eq!(std::fs::read(device.join("value")).unwrap(), b"host-owned");
}
