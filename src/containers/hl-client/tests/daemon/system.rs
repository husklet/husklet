use super::support::*;

#[tokio::test]
async fn system_contract_is_platform_derived_and_unsupported_routes_are_explicit() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let created = containers
        .create(ContainerSpec::from_directory(&rootfs, Process::new("/bin/true")))
        .await
        .unwrap();
    let socket = root.path().join("run/docker.sock");
    let daemon = Daemon::new(containers).platform(Platform::linux_amd64());
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
    let version = client.version().await.unwrap();
    assert_eq!(version.os, "linux");
    assert_eq!(version.arch, "amd64");
    let info = client.system().info().await.unwrap();
    assert_eq!(info.architecture, "amd64");
    assert_eq!(info.containers, 1);
    assert_eq!(info.containers_running, 0);
    assert_eq!(info.containers_stopped, 1);
    assert_eq!(info.containers_paused, 0);
    let usage = client.system().disk_usage().await.unwrap();
    assert_eq!(usage.containers.len(), 1);
    assert!(usage.images.is_empty());
    assert!(usage.volumes.is_empty());

    let error = client.containers().pause(created.id.as_str()).await.unwrap_err();
    assert!(matches!(
        error,
        hl_client::Error::Docker {
            status: http::StatusCode::CONFLICT,
            ..
        }
    ));

    let error = client
        .executions()
        .create(
            created.id.as_str(),
            &hl_client::model::ExecConfig {
                command: vec!["/bin/true".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        hl_client::Error::Docker {
            status: http::StatusCode::CONFLICT,
            ..
        }
    ));

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
