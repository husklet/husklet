//! Docker system disk-usage accounting over raw and typed clients.

use hl_client::{Client, Config as ClientConfig};
use hl_container::{Access, Config, ContainerSpec, Containers, Mount, Process, VolumeSpec};
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

async fn raw_http(socket: &Path) -> Result<String, Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        stream
            .write_all(b"GET /v1.43/system/df HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        if response.len() > 1024 * 1024 {
            return Err("system disk response exceeded one MiB".into());
        }
        String::from_utf8(response).map_err(Into::into)
    })
    .await
    .map_err(|_| "system disk exchange timed out")?
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

fn body(response: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let body = response
        .split_once("\r\n\r\n")
        .ok_or("system disk response omitted its body")?
        .1;
    Ok(serde_json::from_str(body)?)
}

fn require(condition: bool, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    condition.then_some(()).ok_or_else(|| message.into())
}

#[tokio::test]
async fn wire_client() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = work.path().join("rootfs");
    std::fs::create_dir(&rootfs)?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;

    let alpha = containers
        .volumes()
        .create(VolumeSpec::new("alpha").label("purpose", "disk").option("type", "none"))
        .await?;
    std::fs::write(alpha.path().join("payload"), b"alpha")?;
    let zeta = containers.volumes().create(VolumeSpec::new("zeta")).await?;
    std::fs::write(zeta.path().join("payload"), b"zeta-content")?;
    let bind = work.path().join("bind");
    std::fs::create_dir(&bind)?;
    std::fs::write(bind.join("host-owned"), b"excluded")?;
    containers
        .volumes()
        .create(VolumeSpec::new("external").bind(&bind, false))
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(&rootfs, Process::new("/bin/true"))
                .name("alpha-owner")
                .mount(Mount::volume("alpha", "/data", Access::ReadWrite)),
        )
        .await?;

    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let result = async {
        let response = raw_http(&socket).await?;
        require(
            response.starts_with("HTTP/1.1 200"),
            "system disk request was not HTTP 200",
        )?;
        let raw = body(&response)?;
        let raw_volumes = raw["Volumes"]
            .as_array()
            .ok_or("system disk Volumes was not an array")?;
        require(
            raw_volumes
                .iter()
                .map(|volume| volume["Name"].as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                == ["alpha", "external", "zeta"],
            "raw volume accounting was not name ordered",
        )?;
        let alpha_wire = raw_volumes[0].as_object().ok_or("raw alpha volume was not an object")?;
        require(
            alpha_wire
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == [
                    "CreatedAt",
                    "Driver",
                    "Labels",
                    "Mountpoint",
                    "Name",
                    "Options",
                    "Scope",
                    "UsageData",
                ]
                .into_iter()
                .collect(),
            "raw alpha volume did not use the canonical Docker shape",
        )?;
        require(raw_volumes[0]["Driver"] == "local", "raw alpha driver was incorrect")?;
        require(raw_volumes[0]["Scope"] == "local", "raw alpha scope was incorrect")?;
        require(
            raw_volumes[0]["Mountpoint"] == alpha.path().to_string_lossy().as_ref(),
            "raw alpha mountpoint was incorrect",
        )?;
        require(
            raw_volumes[0]["Labels"] == serde_json::json!({"purpose": "disk"}),
            "raw alpha labels were incorrect",
        )?;
        require(
            raw_volumes[0]["Options"] == serde_json::json!({"type": "none"}),
            "raw alpha options were incorrect",
        )?;
        chrono::DateTime::parse_from_rfc3339(
            raw_volumes[0]["CreatedAt"]
                .as_str()
                .ok_or("raw alpha CreatedAt was not a string")?,
        )?;
        require(raw_volumes[0]["UsageData"]["Size"] == 5, "raw alpha size was incorrect")?;
        require(
            raw_volumes[0]["UsageData"]["RefCount"] == 1,
            "raw alpha reference count was incorrect",
        )?;
        require(raw_volumes[1]["UsageData"]["Size"] == 0, "raw bind size was not zero")?;
        require(raw_volumes[2]["UsageData"]["Size"] == 12, "raw zeta size was incorrect")?;

        let client = Client::with_config(ClientConfig::unix(&socket))?;
        let usage = client.system().disk_usage().await?;
        require(
            usage
                .volumes
                .iter()
                .map(|volume| {
                    let usage = volume.usage_data.as_ref();
                    (
                        volume.name.as_str(),
                        usage.map(|usage| usage.size),
                        usage.map(|usage| usage.ref_count),
                    )
                })
                .collect::<Vec<_>>()
                == [
                    ("alpha", Some(5), Some(1)),
                    ("external", Some(0), Some(0)),
                    ("zeta", Some(12), Some(0)),
                ],
            "typed client volume accounting diverged from the raw contract",
        )?;
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = shutdown.send(());
    server.await??;
    result
}
