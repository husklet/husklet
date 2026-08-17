use std::process::Stdio;
use std::time::Duration;

use hl_client::Client;
use hl_container::{Config, ContainerSpec, Containers, Process, Resources};
use tokio::net::UnixStream;
use tokio::process::Command;

#[tokio::test]
async fn process_serves_the_docker_api() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("state");
    let socket = work.path().join("daemon.sock");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_dockerd"))
        .arg("--root")
        .arg(&root)
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            assert!(daemon.try_wait().unwrap().is_none(), "daemon exited before binding");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let client = Client::unix(&socket).unwrap();
    client.ping().await.unwrap();
    assert!(!client.version().await.unwrap().api_version.is_empty());
    assert!(client.containers().list(true).await.unwrap().is_empty());
    daemon.kill().await.unwrap();
    daemon.wait().await.unwrap();
}

#[tokio::test]
async fn process_restart_preserves_container_state() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("state");
    let socket = work.path().join("daemon.sock");
    let rootfs = work.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let identity = Containers::builder(Config::new(&root))
        .build()
        .await
        .unwrap()
        .create(
            ContainerSpec::from_directory(&rootfs, Process::new("/bin/true"))
                .name("durable")
                .resources(Resources {
                    memory_bytes: 64 * 1024 * 1024,
                    process_count: 23,
                    cpu_count: 2,
                    limits: Vec::new(),
                }),
        )
        .await
        .unwrap()
        .id
        .to_string();

    for _ in 0..2 {
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_dockerd"))
            .args(["--root", root.to_str().unwrap(), "--socket", socket.to_str().unwrap()])
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while UnixStream::connect(&socket).await.is_err() {
                assert!(daemon.try_wait().unwrap().is_none(), "daemon exited before binding");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let records = Client::unix(&socket).unwrap().containers().list(true).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].metadata.id, identity);
        let inspected = Client::unix(&socket)
            .unwrap()
            .containers()
            .inspect(&identity)
            .await
            .unwrap();
        assert_eq!(inspected.host_config.memory, 64 * 1024 * 1024);
        assert_eq!(inspected.host_config.pids_limit, Some(23));
        assert_eq!(inspected.host_config.nano_cpus, 2_000_000_000);
        daemon.kill().await.unwrap();
        daemon.wait().await.unwrap();
    }
}

#[tokio::test]
async fn data_root_has_exactly_one_durable_daemon_owner() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("state");
    let first_socket = work.path().join("first.sock");
    let second_socket = work.path().join("second.sock");
    let mut first = Command::new(env!("CARGO_BIN_EXE_dockerd"))
        .args([
            "--root",
            root.to_str().unwrap(),
            "--socket",
            first_socket.to_str().unwrap(),
        ])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while UnixStream::connect(&first_socket).await.is_err() {
            assert!(
                first.try_wait().unwrap().is_none(),
                "first daemon exited before binding"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        Command::new(env!("CARGO_BIN_EXE_dockerd"))
            .args([
                "--root",
                root.to_str().unwrap(),
                "--socket",
                second_socket.to_str().unwrap(),
            ])
            .output(),
    )
    .await
    .expect("competing daemon did not reject ownership")
    .unwrap();

    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("another daemon owns this data root"));
    Client::unix(&first_socket).unwrap().ping().await.unwrap();
    assert!(!second_socket.exists());
    first.kill().await.unwrap();
    first.wait().await.unwrap();
}

#[tokio::test]
async fn successful_checkpoint_publishes_commit_acknowledgement_before_exit() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("state");
    let socket = work.path().join("daemon.sock");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_dockerd"))
        .args(["--root", root.to_str().unwrap(), "--socket", socket.to_str().unwrap()])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while UnixStream::connect(&socket).await.is_err() {
            assert!(daemon.try_wait().unwrap().is_none(), "daemon exited before binding");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let signal = Command::new("/bin/kill")
        .args(["-HUP", &daemon.id().unwrap().to_string()])
        .status()
        .await
        .unwrap();
    assert!(signal.success());
    let status = tokio::time::timeout(Duration::from_secs(15), daemon.wait())
        .await
        .expect("checkpointed daemon did not exit")
        .unwrap();

    assert!(status.success());
    assert_eq!(std::fs::read(root.join("shutdown.success")).unwrap(), b"ok\n");
    assert!(!root.join("shutdown.error").exists());
}
