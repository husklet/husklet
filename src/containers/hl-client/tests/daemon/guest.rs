use super::support::*;

#[tokio::test]
async fn extra_hosts_are_resolved_from_the_guest_hosts_file() {
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
            runnable_daemon(containers)
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
                    "grep -E '^203\\.0\\.113\\.9[[:space:]]+database$' /etc/hosts".into(),
                ]),
                host_config: Some(hl_client::model::HostConfig {
                    extra_hosts: vec!["database:203.0.113.9".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some("extra-hosts"),
        )
        .await
        .unwrap();
    client.containers().start(&created.id).await.unwrap();
    assert_eq!(client.containers().wait(&created.id).await.unwrap().status_code, 0);
    assert_eq!(
        client.containers().logs(&created.id, true, true).await.unwrap().stdout,
        b"203.0.113.9\tdatabase\n"
    );
    assert_eq!(
        client
            .containers()
            .inspect(&created.id)
            .await
            .unwrap()
            .host_config
            .extra_hosts,
        ["database:203.0.113.9"]
    );

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
