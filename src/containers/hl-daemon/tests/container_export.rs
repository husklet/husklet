//! Docker-compatible container export response metadata and lookup behavior.

use hl_container::{Config, ContainerSpec, Containers, Process};
use hl_daemon::Daemon;
use std::{path::Path, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
    sync::oneshot,
    time::{sleep, timeout},
};

const TIMEOUT: Duration = Duration::from_secs(15);

async fn raw_http(socket: &Path, request: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(request).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        if response.len() > 1024 * 1024 {
            return Err("container export response exceeded one MiB".into());
        }
        String::from_utf8(response).map_err(Into::into)
    })
    .await
    .map_err(|_| "raw HTTP exchange timed out")?
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

fn require(condition: bool, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    condition.then_some(()).ok_or_else(|| message.into())
}

#[tokio::test]
async fn wire_contract() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = work.path().join("rootfs");
    std::fs::create_dir(&rootfs)?;
    std::fs::write(rootfs.join("export-marker"), b"root-filesystem")?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let container = containers
        .create(ContainerSpec::from_directory(&rootfs, Process::new("/bin/true")))
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let missing = raw_http(
        &socket,
        b"GET /v1.43/containers/missing/export HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    require(missing.starts_with("HTTP/1.1 404"), "missing export was not a 404")?;

    let request = format!(
        "GET /v1.43/containers/{}/export HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        container.id
    );
    let response = raw_http(&socket, request.as_bytes()).await?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or("export response omitted the HTTP header boundary")?;
    let headers = headers.to_ascii_lowercase();
    require(
        headers.starts_with("http/1.1 200"),
        "container export was not successful",
    )?;
    require(
        headers.contains("content-type: application/octet-stream"),
        "container export did not use Docker's octet-stream media type",
    )?;
    require(
        !headers.contains("x-docker-container-path-stat:"),
        "container export leaked copy-from path metadata",
    )?;
    require(
        !headers.contains("content-disposition:"),
        "container export added a Content-Disposition header absent from Moby",
    )?;
    require(
        body.contains("export-marker"),
        "container export omitted the root filesystem",
    )?;

    let _ = shutdown.send(());
    server.await??;
    Ok(())
}
