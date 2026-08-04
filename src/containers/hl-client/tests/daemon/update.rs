use super::support::*;

#[tokio::test]
async fn container_update_persists_effective_settings_and_rejects_unknown_fields() {
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
            Some("update-source"),
        )
        .await
        .unwrap();
    let result = client
        .containers()
        .update(
            &created.id,
            &hl_client::model::Update {
                memory: Some(8192),
                pids_limit: Some(12),
                nano_cpus: Some(1_500_000_000),
                restart_policy: Some(hl_client::model::RestartPolicy {
                    name: "on-failure".into(),
                    maximum_retry_count: 3,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(result.warnings.is_empty());
    let stored = containers.inspect(&created.id).await.unwrap();
    assert_eq!(stored.spec.resources.memory_bytes, 8192);
    assert_eq!(stored.spec.resources.process_count, 12);
    assert_eq!(stored.spec.resources.cpu_count, 2);
    assert_eq!(
        stored.spec.restart,
        hl_container::RestartPolicy::OnFailure { maximum: Some(3) }
    );

    let unsupported = client
        .containers()
        .update(
            &created.id,
            &hl_client::model::Update {
                unsupported: [("CpuShares".into(), serde_json::json!(128))].into_iter().collect(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        unsupported,
        hl_client::Error::Docker {
            status: http::StatusCode::BAD_REQUEST,
            ..
        }
    ));

    client.containers().remove(&created.id, false, false).await.unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
