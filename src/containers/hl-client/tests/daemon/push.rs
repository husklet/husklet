use super::support::*;

#[tokio::test]
async fn registry_push_streams_typed_errors_and_rejects_malformed_auth() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &docker_archive()[..],
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
    let mut push = client
        .images()
        .push("missing/image", Some("v1"), None)
        .await
        .unwrap();
    let error = push.next().await.unwrap().unwrap();
    assert!(error.error.unwrap().contains("missing/image:v1"));
    assert!(error.error_detail.is_some());
    assert!(push.next().await.unwrap().is_none());

    let malformed = raw_http(
        &socket,
        b"POST /v1.43/images/scenario%2Ffixture/push?tag=v1 HTTP/1.1\r\nHost: localhost\r\nX-Registry-Auth: not-base64!\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(malformed.starts_with("HTTP/1.1 200"));
    assert!(malformed.contains("invalid X-Registry-Auth header"));
    assert!(malformed.contains("errorDetail"));

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
