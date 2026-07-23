use super::support::*;

#[tokio::test]
async fn prune_removes_inactive_containers_through_shared_headless_ownership() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let first = containers
        .create(ContainerSpec::from_directory(root.path(), Process::new("/bin/true")).name("first"))
        .await
        .unwrap();
    let second = containers
        .create(
            ContainerSpec::from_directory(root.path(), Process::new("/bin/true")).name("second"),
        )
        .await
        .unwrap();
    let daemon = Daemon::new(containers);
    let socket = root.path().join("prune.sock");
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
    let mut deleted = client
        .containers()
        .prune()
        .await
        .unwrap()
        .containers_deleted;
    deleted.sort();
    let mut expected = vec![first.id.to_string(), second.id.to_string()];
    expected.sort();
    assert_eq!(deleted, expected);
    assert_eq!(client.containers().list(true).await.unwrap(), Vec::new());
    assert!(daemon
        .headless()
        .containers()
        .list()
        .await
        .unwrap()
        .is_empty());

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn filtered_prune_selects_containers_and_system_resources() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let daemon = Daemon::new(containers);
    let socket = root.path().join("filtered-prune.sock");
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

    daemon
        .headless()
        .containers()
        .create(
            ContainerSpec::from_directory(root.path(), Process::new("/bin/true"))
                .name("keep")
                .label("keep", "true"),
        )
        .await
        .unwrap();
    let remove = daemon
        .headless()
        .containers()
        .create(
            ContainerSpec::from_directory(root.path(), Process::new("/bin/true"))
                .name("remove")
                .label("keep", "false"),
        )
        .await
        .unwrap();
    let filtered = client
        .containers()
        .prune_with(&[("label".into(), vec!["keep=false".into()])].into())
        .await
        .unwrap();
    assert_eq!(filtered.containers_deleted, [remove.id.to_string()]);
    assert!(daemon.headless().containers().inspect("keep").await.is_ok());

    let system_remove = daemon
        .headless()
        .containers()
        .create(
            ContainerSpec::from_directory(root.path(), Process::new("/bin/true"))
                .name("system-remove")
                .label("keep", "false"),
        )
        .await
        .unwrap();
    let filtered = client
        .system()
        .prune_with(false, &[("label!".into(), vec!["keep=true".into()])].into())
        .await
        .unwrap();
    assert_eq!(filtered.containers_deleted, [system_remove.id.to_string()]);
    assert!(daemon.headless().containers().inspect("keep").await.is_ok());
    assert!(daemon
        .headless()
        .containers()
        .inspect(system_remove.id.as_str())
        .await
        .is_err());

    let unsupported = raw_http(
        &socket,
        b"POST /v1.43/containers/prune?filters=%7B%22status%22%3A%5B%22exited%22%5D%7D HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(unsupported.starts_with("HTTP/1.1 400"));

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
