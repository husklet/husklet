use super::support::*;

#[tokio::test]
async fn observability_client_exposes_stats_and_rejects_top_for_inactive_processes() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let container = containers
        .create(ContainerSpec::from_directory(
            root.path(),
            Process::new("/bin/sleep").args(["60"]),
        ))
        .await
        .unwrap();
    let socket = root.path().join("observe.sock");
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_socket(&socket).await;
    let client = Client::unix(&socket).unwrap();

    let stats = client.containers().stats(container.id.as_str()).await.unwrap();
    assert_eq!(stats.id, container.id.as_str());
    assert_eq!(stats.pids_stats.current, 0);
    assert_eq!(stats.num_procs, 0);
    assert_eq!(stats.memory_stats.usage, 0);
    let mut stream = client.containers().stats_stream(container.id.as_str()).await.unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap().id, container.id.as_str());
    let next = stream.next().await.unwrap().unwrap();
    assert_eq!(next.id, container.id.as_str());
    assert_eq!(next.pids_stats.current, 0);
    drop(stream);
    assert!(client.containers().top(container.id.as_str()).await.is_err());
    let error = client
        .containers()
        .top_with(container.id.as_str(), Some("-eo pid,unsupported"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unsupported top column"));

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn observability_client_reports_a_live_process_and_resource_sample() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &runnable_archive()[..],
        &containers.images().unwrap(),
        Limits::default(),
    )
    .unwrap();
    let socket = root.path().join("live-observe.sock");
    let daemon = TestDaemon::start(containers, &socket).await;
    let client = &daemon.client;
    let created = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/runnable:v1".into(),
                entrypoint: Some(vec!["/bin/sleep".into()]),
                cmd: Some(vec!["60".into()]),
                ..Default::default()
            },
            Some("live-observe"),
        )
        .await
        .unwrap();
    client.containers().start(&created.id).await.unwrap();

    let top = client.containers().top(&created.id).await.unwrap();
    assert!(top.titles.iter().any(|title| title == "PID"));
    assert_eq!(top.processes.len(), 1);
    assert!(top.processes[0].iter().any(|field| field.contains("sleep")));
    let stats = client.containers().stats(&created.id).await.unwrap();
    assert_eq!(stats.id, created.id);
    assert_eq!(stats.name, "/live-observe");
    assert_eq!(stats.pids_stats.current, 1);
    assert_eq!(stats.num_procs, 1);

    client.containers().remove(&created.id, true, false).await.unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn container_archive_round_trip_streams_through_typed_client() {
    let root = TempDir::new().unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir_all(rootfs.join("inbox")).unwrap();
    std::fs::write(rootfs.join("source"), b"from-container").unwrap();
    let containers = containers(&root).await;
    let created = containers
        .create(ContainerSpec::from_directory(&rootfs, Process::new("/bin/true")))
        .await
        .unwrap();
    let socket = root.path().join("run/docker.sock");
    let daemon = TestDaemon::start(containers, &socket).await;
    let client = &daemon.client;

    for error in [
        client.containers().stat("missing", "/source").await.unwrap_err(),
        client.containers().copy_from("missing", "/source").await.unwrap_err(),
    ] {
        assert!(matches!(
            error,
            hl_client::Error::Docker {
                status: http::StatusCode::NOT_FOUND,
                ..
            }
        ));
    }

    let stat = client.containers().stat(created.id.as_str(), "/source").await.unwrap();
    assert_eq!(stat.name, "source");
    assert_eq!(stat.size, 14);

    let mut archive = client
        .containers()
        .copy_from(created.id.as_str(), "/source")
        .await
        .unwrap()
        .into_stream();
    let mut downloaded = Vec::new();
    while let Some(chunk) = archive.next_chunk().await.unwrap() {
        downloaded.extend_from_slice(&chunk);
    }
    let mut downloaded_archive = tar::Archive::new(&downloaded[..]);
    let mut entries = downloaded_archive.entries().unwrap();
    let mut entry = entries.next().unwrap().unwrap();
    assert_eq!(entry.path().unwrap(), std::path::Path::new("source"));
    let mut contents = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut contents).unwrap();
    assert_eq!(contents, b"from-container");

    let uploaded = docker_tar(&[("nested/copied", b"to-container")]);
    client
        .containers()
        .copy_to(created.id.as_str(), "/inbox", std::io::Cursor::new(uploaded))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(rootfs.join("inbox/nested/copied")).unwrap(),
        b"to-container"
    );

    let mut exported = client.containers().export(created.id.as_str()).await.unwrap();
    let mut export_bytes = Vec::new();
    while let Some(chunk) = exported.next_chunk().await.unwrap() {
        export_bytes.extend_from_slice(&chunk);
    }
    let names = tar::Archive::new(&export_bytes[..])
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert!(names.iter().any(|path| path.ends_with("source")));
    assert!(names.iter().any(|path| path.ends_with("inbox/nested/copied")));

    daemon.stop().await;
}
