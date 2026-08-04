//! Docker container-summary size accounting and wire omission contracts.

use hl_container::{Config, ContainerSpec, Containers, Process};
use hl_daemon::Daemon;
use serde_json::Value;
use std::{path::Path, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
    sync::oneshot,
    time::{sleep, timeout},
};

const TIMEOUT: Duration = Duration::from_secs(15);

async fn exchange(socket: &Path, target: &str) -> Result<String, Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        let request = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        if response.len() > 1024 * 1024 {
            return Err("container-size response exceeded one MiB".into());
        }
        String::from_utf8(response).map_err(Into::into)
    })
    .await
    .map_err(|_| "container-size request timed out")?
}

async fn request(socket: &Path, target: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let response = exchange(socket, target).await?;
    let (headers, body) = response.split_once("\r\n\r\n").ok_or("missing HTTP body")?;
    if !headers.starts_with("HTTP/1.1 200") {
        return Err(format!("container-size request failed: {headers}").into());
    }
    serde_json::from_str(body).map_err(Into::into)
}

async fn wait_for_path(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        while !socket.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "daemon socket startup timed out".into())
}

#[tokio::test]
async fn wire_contract() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let old_root = work.path().join("old_root");
    let new_root = work.path().join("new_root");
    std::fs::create_dir(&old_root)?;
    std::fs::create_dir(&new_root)?;
    std::fs::write(old_root.join("old"), b"old-root")?;
    std::fs::write(new_root.join("new"), b"new-root-filesystem")?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    containers
        .create(ContainerSpec::from_directory(&old_root, Process::new("/bin/true")).name("old"))
        .await?;
    sleep(Duration::from_millis(2)).await;
    containers
        .create(ContainerSpec::from_directory(&new_root, Process::new("/bin/true")).name("selected"))
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    for flag in ["0", "false"] {
        let ordinary = request(&socket, &format!("/v1.43/containers/json?all=true&size={flag}")).await?;
        for container in ordinary.as_array().ok_or("container list was not an array")? {
            assert!(container.get("SizeRw").is_none());
            assert!(container.get("SizeRootFs").is_none());
        }
    }
    let sized = request(&socket, "/v1.43/containers/json?all=true&size=1&limit=1").await?;
    let sized = sized.as_array().ok_or("sized list was not an array")?;
    assert_eq!(sized.len(), 1);
    assert_eq!(sized[0]["SizeRw"], 0);
    assert!(sized[0]["SizeRootFs"].as_u64().is_some_and(|size| size > 0));
    let invalid = exchange(&socket, "/v1.43/containers/json?all=true&size=maybe").await?;
    assert!(invalid.starts_with("HTTP/1.1 400"));

    let disk = request(&socket, "/v1.43/system/df").await?;
    let disk_containers = disk["Containers"]
        .as_array()
        .ok_or("disk containers were not an array")?;
    assert_eq!(disk_containers.len(), 2);
    for container in disk_containers {
        assert_eq!(container["SizeRw"], 0);
        assert!(container["SizeRootFs"].as_u64().is_some_and(|size| size > 0));
    }

    std::fs::remove_dir_all(&old_root)?;
    let selected = request(
        &socket,
        "/v1.43/containers/json?all=true&size=true&limit=1&filters=%7B%22name%22%3A%5B%22selected%22%5D%7D",
    )
    .await?;
    let selected = selected.as_array().ok_or("sized list was not an array")?;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["Names"], serde_json::json!(["/selected"]));
    assert_eq!(selected[0]["SizeRw"], 0);
    assert!(selected[0]["SizeRootFs"].as_u64().is_some_and(|size| size > 0));

    let _ = shutdown.send(());
    server.await??;
    Ok(())
}
