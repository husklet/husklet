use super::support::*;

#[tokio::test]
async fn anonymous_registry_pull_streams_progress_and_publishes_atomically() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let socket = root.path().join("run/docker.sock");
    let daemon = Daemon::new(containers.clone()).image_source(registry_fixture());
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
    let mut pull = client
        .images()
        .pull("registry.test/team/demo", Some("v2"), Some("linux/arm64"))
        .await
        .unwrap();
    let first = pull.next().await.unwrap().unwrap();
    assert!(first.status.unwrap().starts_with("Pulling from "));
    let completed = pull.next().await.unwrap().unwrap();
    assert!(completed.error.is_none());
    assert!(completed.status.unwrap().contains("downloaded newer image"));
    assert!(pull.next().await.unwrap().is_none());

    let inspected = client
        .images()
        .inspect("registry.test/team/demo:v2")
        .await
        .unwrap();
    assert_eq!(
        inspected.repo_tags,
        ["registry.test/team/demo:v2".to_owned()]
    );
    assert!(containers
        .images()
        .unwrap()
        .leases()
        .list()
        .unwrap()
        .is_empty());

    let invalid = client
        .images()
        .pull("registry.test/team/demo", None, Some("darwin/arm64"))
        .await
        .unwrap_err();
    assert!(matches!(
        invalid,
        hl_client::Error::Docker {
            status: http::StatusCode::BAD_REQUEST,
            ..
        }
    ));

    let authenticated = raw_http(
        &socket,
        b"POST /v1.43/images/create?fromImage=registry.test%2Fteam%2Fdemo HTTP/1.1\r\nHost: localhost\r\nX-Registry-Auth: e30=\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(authenticated.starts_with("HTTP/1.1 200"));
    assert!(!authenticated.contains("registry authentication is not implemented"));

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn authenticated_registry_pull_validates_and_never_leaks_credentials() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let socket = root.path().join("authenticated-pull.sock");
    let (image, registry) = basic_registry("puller", "s3cret-value").await;
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
    let credentials = hl_client::model::Credentials {
        username: "puller".into(),
        password: "s3cret-value".into(),
        ..Default::default()
    };
    let mut pull = client
        .images()
        .pull_with(&image, None, Some("linux/arm64"), Some(&credentials))
        .await
        .unwrap();
    let mut records = Vec::new();
    while let Some(record) = pull.next().await.unwrap() {
        records.push(record);
    }
    let rendered = serde_json::to_string(&records).unwrap();
    assert!(
        records.iter().all(|record| record.error.is_none()),
        "{rendered}"
    );
    assert!(!rendered.contains("s3cret-value"));

    let rejected_credentials = hl_client::model::Credentials {
        username: "puller".into(),
        password: "wrong-secret".into(),
        ..Default::default()
    };
    let mut rejected = client
        .images()
        .pull_with(
            &image,
            None,
            Some("linux/arm64"),
            Some(&rejected_credentials),
        )
        .await
        .unwrap();
    let mut rejection = Vec::new();
    while let Some(record) = rejected.next().await.unwrap() {
        rejection.push(record);
    }
    let rejection = serde_json::to_string(&rejection).unwrap();
    assert!(rejection.contains("Not authorized"));
    assert!(!rejection.contains("wrong-secret"));

    let invalid = raw_http(&socket, b"POST /v1.43/images/create?fromImage=example.invalid%2Fdemo HTTP/1.1\r\nHost: localhost\r\nX-Registry-Auth: not-base64!\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    assert!(invalid.starts_with("HTTP/1.1 400"));
    assert!(!invalid.contains("s3cret-value"));

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    registry.abort();
}
