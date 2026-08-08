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
