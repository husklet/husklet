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

    let ping = raw_http(
        &socket,
        b"GET /_ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    for header in [
        "api-version: 1.43",
        "builder-version: 2",
        "docker-experimental: false",
        "ostype: linux",
        "swarm: inactive",
        "cache-control: no-cache, no-store, must-revalidate",
        "pragma: no-cache",
    ] {
        assert!(ping.to_ascii_lowercase().contains(header), "missing {header}: {ping}");
    }
    assert!(ping.ends_with("\r\n\r\nOK"));

    let head = raw_http(
        &socket,
        b"HEAD /_ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(head.to_ascii_lowercase().contains("api-version: 1.43"));
    assert!(head.to_ascii_lowercase().contains("content-length: 0"));
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/plain; charset=utf-8")
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("cache-control: no-cache, no-store, must-revalidate")
    );
    assert!(head.to_ascii_lowercase().contains("pragma: no-cache"));
    assert!(head.ends_with("\r\n\r\n"));
    assert!(!head.ends_with("OK"));

    for (method, status) in [("GET", "200"), ("HEAD", "200"), ("POST", "405")] {
        let request = format!(
            "{method} /v1.43/_ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
        );
        let response = raw_http(&socket, request.as_bytes()).await;
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{method}: {response}"
        );
        if method == "GET" {
            assert!(response.to_ascii_lowercase().contains("content-length: 2"));
            assert!(response.ends_with("\r\n\r\nOK"));
        } else if method == "HEAD" {
            assert!(response.to_ascii_lowercase().contains("content-length: 0"));
            assert!(
                response
                    .to_ascii_lowercase()
                    .contains("content-type: text/plain; charset=utf-8")
            );
            assert!(response.ends_with("\r\n\r\n"));
        }
    }

    let client = Client::unix(&socket).unwrap();
    client.ping().await.unwrap();
    let version = client.version().await.unwrap();
    assert_eq!(version.os, "linux");
    assert_eq!(version.arch, "amd64");
    let selected_version = raw_http(
        &socket,
        b"GET /v1.24/version HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(selected_version.starts_with("HTTP/1.1 200"), "{selected_version}");
    assert!(selected_version.contains("\"ApiVersion\":\"1.43\""));
    assert!(selected_version.contains("\"MinAPIVersion\":\"1.24\""));
    let numeric_flag = raw_http(
        &socket,
        b"GET /v1.43/containers/json?all=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(numeric_flag.starts_with("HTTP/1.1 200"), "{numeric_flag}");
    // Docker's BoolValue treats only "", 0, no, false and none as false and never rejects a
    // spelling, so `all=yes` is true rather than a client error. Measured against 29.1.3.
    let truthy_flag = raw_http(
        &socket,
        b"GET /v1.43/containers/json?all=yes HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(truthy_flag.starts_with("HTTP/1.1 200"), "{truthy_flag}");
    let falsey_flag = raw_http(
        &socket,
        b"GET /v1.43/containers/json?all=none HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(falsey_flag.starts_with("HTTP/1.1 200"), "{falsey_flag}");
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
