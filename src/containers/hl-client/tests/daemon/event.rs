use super::support::*;

#[tokio::test]
async fn typed_event_stream_replays_create_and_destroy_from_real_handlers() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &docker_archive()[..],
        &containers.images().unwrap(),
        Limits::default(),
    )
    .unwrap();
    let daemon = Daemon::new(containers);
    let socket = root.path().join("run/docker.sock");
    let (stop, stopped) = oneshot::channel();
    let server = daemon.server(&socket);
    let task = tokio::spawn(async move {
        server
            .serve_with_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    wait_for_socket(&socket).await;

    let client = Client::unix(&socket).unwrap();
    let created = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.containers().create(
            &hl_client::model::CreateContainer {
                image: "scenario/fixture:v1".into(),
                labels: [("tier".into(), "api".into())].into_iter().collect(),
                ..Default::default()
            },
            Some("event-source"),
        ),
    )
    .await
    .expect("event source create timed out")
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.containers().remove(&created.id, false, false),
    )
    .await
    .expect("event source remove timed out")
    .unwrap();

    let query = EventQuery::default().filters(
        EventFilter::default()
            .container(&created.id)
            .label("tier=api"),
    );
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.events().subscribe(&query),
    )
    .await
    .expect("event subscription response timed out")
    .unwrap();
    let create = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("create event timed out")
        .unwrap()
        .unwrap();
    let destroy = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("destroy event timed out")
        .unwrap()
        .unwrap();
    assert_eq!(
        (create.action.as_str(), destroy.action.as_str()),
        ("create", "destroy")
    );
    assert_eq!(create.actor.id, created.id);
    assert_eq!(create.actor.attributes.get("name").unwrap(), "event-source");

    drop(stream);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn client_reaches_real_server_and_observes_headless_state() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    containers
        .create(ContainerSpec::from_directory("/rootfs", Process::new("/bin/true")).name("direct"))
        .await
        .unwrap();
    let daemon = Daemon::new(containers);
    assert_eq!(
        daemon.headless().containers().list().await.unwrap().len(),
        1
    );

    let socket = root.path().join("run/docker.sock");
    let (stop, stopped) = oneshot::channel();
    let server = daemon.server(&socket);
    let task = tokio::spawn(async move {
        server
            .serve_with_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    wait_for_socket(&socket).await;

    let client = Client::unix(&socket).unwrap();
    client.ping().await.unwrap();
    assert_eq!(client.version().await.unwrap().api_version, "1.43");
    let listed = client.containers().list(true).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].names, vec!["/direct"]);
    client
        .containers()
        .rename(&listed[0].metadata.id, "renamed")
        .await
        .unwrap();
    let inspected = client.containers().inspect("renamed").await.unwrap();
    assert_eq!(inspected.name, "/renamed");
    client.containers().stop("renamed", Some(0)).await.unwrap();
    let logs = client
        .containers()
        .logs("renamed", true, true)
        .await
        .unwrap();
    assert!(logs.stdout.is_empty());
    assert!(logs.stderr.is_empty());
    let waiter = Client::unix(&socket).unwrap();
    let id = listed[0].metadata.id.clone();
    let removed = tokio::spawn(async move {
        waiter
            .containers()
            .wait_for(&id, WaitCondition::Removed)
            .await
    });
    tokio::task::yield_now().await;
    client
        .containers()
        .remove("renamed", false, false)
        .await
        .unwrap();
    assert_eq!(removed.await.unwrap().unwrap().status_code, 0);

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!socket.exists());
}
