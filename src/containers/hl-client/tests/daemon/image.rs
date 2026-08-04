use super::support::*;

#[tokio::test]
async fn image_archive_round_trip_uses_shared_wire_contracts() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let socket = root.path().join("run/docker.sock");
    let daemon = TestDaemon::start(containers.clone(), &socket).await;

    let archive_path = root.path().join("image.tar");
    let archive = docker_archive();
    let expected_id = {
        let mut tar = tar::Archive::new(&archive[..]);
        let mut config = Vec::new();
        tar.entries()
            .unwrap()
            .find(|entry| entry.as_ref().unwrap().path().unwrap() == std::path::Path::new("config.json"))
            .unwrap()
            .unwrap()
            .read_to_end(&mut config)
            .unwrap();
        Digest::sha256(&config).to_string()
    };
    tokio::fs::write(&archive_path, &archive).await.unwrap();
    let client = &daemon.client;
    let quiet = client
        .images()
        .load_with(tokio::fs::File::open(&archive_path).await.unwrap(), true)
        .await
        .unwrap();
    assert_eq!(quiet.stream, "");
    assert_eq!(client.images().list().await.unwrap().len(), 1);

    let loaded = client
        .images()
        .load(tokio::fs::File::open(&archive_path).await.unwrap())
        .await
        .unwrap();
    assert_eq!(loaded.stream, "Loaded image: docker.io/scenario/fixture:v1\n");
    let listed = client.images().list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].repo_tags, ["docker.io/scenario/fixture:v1"]);

    let inspected = client.images().inspect("scenario/fixture:v1").await.unwrap();
    assert_eq!(inspected.id, listed[0].id);
    assert_eq!(inspected.id, expected_id);
    assert_eq!(inspected.os, "linux");
    assert_eq!(inspected.architecture, "arm64");
    assert_eq!(inspected.created, "2026-07-15T12:34:56Z");
    let distribution = client.images().distribution("scenario/fixture:v1").await.unwrap();
    assert_ne!(distribution.descriptor.digest().to_string(), inspected.id);
    assert_eq!(distribution.platforms, [Platform::linux_arm64()]);
    let history = client.images().history("scenario/fixture:v1").await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, inspected.id);
    assert_eq!(history[0].created, 1_784_118_896);
    assert_eq!(history[0].created_by, "/bin/sh -c #(nop) ADD fixture");
    assert_eq!(history[0].comment, "integration fixture");
    assert_eq!(history[0].tags, ["docker.io/scenario/fixture:v1"]);
    client
        .images()
        .tag("scenario/fixture:v1", "scenario/stable", Some("v1"))
        .await
        .unwrap();
    assert_eq!(
        client.images().inspect("scenario/stable:v1").await.unwrap().id,
        expected_id
    );
    daemon.stop().await;

    let reopened = TestDaemon::start(containers, &socket).await;
    assert_eq!(
        reopened.client.images().inspect("scenario/stable:v1").await.unwrap().id,
        expected_id
    );
    let listed = reopened.client.images().list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, expected_id);
    assert_eq!(listed[0].repo_tags.len(), 2);
    reopened.stop().await;
}

#[tokio::test]
async fn image_archive_tag_save_remove_and_prune_share_wire_contracts() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    let socket = root.path().join("run/docker.sock");
    let daemon = TestDaemon::start(containers, &socket).await;
    let client = &daemon.client;
    client
        .images()
        .load(std::io::Cursor::new(docker_archive()))
        .await
        .unwrap();
    client
        .images()
        .tag("scenario/fixture:v1", "scenario/copy", Some("v2"))
        .await
        .unwrap();
    assert_eq!(client.images().list().await.unwrap()[0].repo_tags.len(), 2);
    for (error, status) in [
        (
            client
                .images()
                .tag("scenario/fixture:v1", "", Some("v2"))
                .await
                .unwrap_err(),
            http::StatusCode::BAD_REQUEST,
        ),
        (
            client
                .images()
                .tag("scenario/missing:v1", "scenario/copy", Some("v2"))
                .await
                .unwrap_err(),
            http::StatusCode::NOT_FOUND,
        ),
    ] {
        assert!(matches!(
            error,
            hl_client::Error::Docker { status: actual, .. } if actual == status
        ));
    }

    let mut archive = client.images().save(&["docker.io/scenario/copy:v2"]).await.unwrap();
    let mut saved = Vec::new();
    while let Some(chunk) = archive.next_chunk().await.unwrap() {
        saved.extend_from_slice(&chunk);
    }
    let mut tar = tar::Archive::new(&saved[..]);
    let paths: Vec<_> = tar
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect();
    assert!(paths.iter().any(|path| path == std::path::Path::new("manifest.json")));

    let removed = client.images().remove("scenario/copy:v2").await.unwrap();
    assert_eq!(removed[0].untagged.as_deref(), Some("docker.io/scenario/copy:v2"));
    assert_eq!(client.images().list().await.unwrap()[0].repo_tags.len(), 1);
    let pruned = client.images().prune().await.unwrap();
    assert!(pruned.images_deleted.is_empty());
    assert_eq!(pruned.space_reclaimed, 0);

    daemon.stop().await;
}

#[tokio::test]
async fn build_accepts_the_standard_bridge_network_mode() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(&docker_archive()[..], &containers.images().unwrap(), Limits::default()).unwrap();
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
    let id = client
        .images()
        .build_with_network(
            std::io::Cursor::new(dockerfile_context(
                b"FROM scenario/fixture:v1\nLABEL test.network=bridge\n",
            )),
            "scenario/bridge-build:v1",
            None,
            "bridge",
        )
        .await
        .unwrap();
    assert!(id.starts_with("sha256:"));
    let image = client.images().inspect("scenario/bridge-build:v1").await.unwrap();
    assert_eq!(image.config.labels["test.network"], "bridge");

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn build_accepts_an_existing_named_network_and_rejects_a_missing_one() {
    let root = TempDir::new().unwrap();
    let containers = containers(&root).await;
    Archive::load(
        &runnable_archive()[..],
        &containers.images().unwrap(),
        Limits::default(),
    )
    .unwrap();
    containers
        .networks()
        .create(hl_container::NetworkSpec::bridge(
            "build-backend",
            hl_container::Subnet::new("10.77.0.0".parse().unwrap(), 24).unwrap(),
        ))
        .await
        .unwrap();
    let socket = root.path().join("run/docker.sock");
    let daemon = TestDaemon::start(containers, &socket).await;
    let client = &daemon.client;
    let context = || {
        std::io::Cursor::new(dockerfile_context(
            b"FROM scenario/runnable:v1\nRUN cat /etc/hosts > /network.txt\nLABEL test.network=custom\nCMD cat /network.txt\n",
        ))
    };
    let id = client
        .images()
        .build_with_network(context(), "scenario/named-network-build:v1", None, "build-backend")
        .await
        .unwrap();
    assert!(id.starts_with("sha256:"));
    assert_eq!(
        client
            .images()
            .inspect("scenario/named-network-build:v1")
            .await
            .unwrap()
            .config
            .labels["test.network"],
        "custom"
    );
    let container = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/named-network-build:v1".into(),
                ..Default::default()
            },
            Some("named-network-build-result"),
        )
        .await
        .unwrap();
    client.containers().start(&container.id).await.unwrap();
    let status = client.containers().wait(&container.id).await.unwrap();
    assert_eq!(status.status_code, 0);
    let logs = client.containers().logs(&container.id, true, true).await.unwrap();
    assert!(
        String::from_utf8_lossy(&logs.stdout).contains("10.77.0."),
        "RUN network snapshot did not contain the named subnet: {:?}",
        String::from_utf8_lossy(&logs.stdout)
    );
    client.containers().remove(&container.id, false, false).await.unwrap();

    let error = client
        .images()
        .build_with_network(context(), "scenario/missing-network:v1", None, "missing")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        hl_client::Error::Docker {
            status: http::StatusCode::BAD_REQUEST,
            ..
        }
    ));
    assert!(client.images().inspect("scenario/missing-network:v1").await.is_err());
    assert!(client.containers().list(true).await.unwrap().is_empty());

    daemon.stop().await;
}
