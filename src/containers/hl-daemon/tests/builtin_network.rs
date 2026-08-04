//! Docker built-in network lifecycle over the raw daemon wire.

use hl_container::{Config, Containers, Subnet};
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

async fn exchange(socket: &Path, request: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(request).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        if response.len() > 1024 * 1024 {
            return Err("network response exceeded one MiB".into());
        }
        String::from_utf8(response).map_err(Into::into)
    })
    .await
    .map_err(|_| "network exchange timed out")?
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
    let value = response
        .split_once("\r\n\r\n")
        .ok_or("HTTP response omitted its body")?
        .1;
    Ok(serde_json::from_str(value)?)
}

fn require(condition: bool, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    condition.then_some(()).ok_or_else(|| message.into())
}

#[tokio::test]
async fn wire_contract() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let config = Config::new(work.path().join("state"));
    let containers = Containers::builder(config.clone()).build().await?;
    let initial = containers.networks().list().await?;
    require(
        initial.len() == 2,
        "assembly did not create exactly two built-in networks",
    )?;
    let bridge = initial
        .iter()
        .find(|network| network.name == "bridge")
        .ok_or("assembly omitted the bridge network")?;
    let none = initial
        .iter()
        .find(|network| network.name == "none")
        .ok_or("assembly omitted the none network")?;
    require(
        bridge.subnet == Some(Subnet::new("172.17.0.0".parse()?, 16)?),
        "built-in bridge did not use Docker's default IPv4 subnet",
    )?;
    require(
        bridge.gateway == Some("172.17.0.1".parse()?),
        "built-in bridge did not reserve Docker's default gateway",
    )?;
    require(none.subnet.is_none(), "built-in none network carried IPAM")?;
    let identities = (bridge.id.clone(), none.id.clone());

    drop(containers);
    let containers = Containers::builder(config).build().await?;
    require(
        containers.networks().inspect("bridge").await?.id == identities.0,
        "bridge identity changed across restart",
    )?;
    require(
        containers.networks().inspect("none").await?.id == identities.1,
        "none identity changed across restart",
    )?;

    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let result = async {
        let listed = exchange(
            &socket,
            b"GET /v1.43/networks HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        require(listed.starts_with("HTTP/1.1 200"), "network list was not HTTP 200")?;
        let listed_body = body(&listed)?;
        let networks = listed_body
            .as_array()
            .ok_or("network list was not a JSON array")?
            .iter()
            .map(|network| {
                let name = network["Name"].as_str().ok_or("network omitted Name")?;
                let driver = network["Driver"].as_str().ok_or("network omitted Driver")?;
                Ok::<_, Box<dyn std::error::Error>>((name.to_owned(), driver.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        require(
            networks == [("bridge".into(), "bridge".into()), ("none".into(), "null".into())],
            "Docker list did not expose Moby built-in names and drivers",
        )?;
        let bridge = listed_body
            .as_array()
            .and_then(|networks| networks.iter().find(|network| network["Name"] == "bridge"))
            .ok_or("Docker list omitted its built-in bridge")?;
        require(
            bridge["IPAM"]["Config"][0]["Subnet"] == "172.17.0.0/16",
            "Docker list exposed the wrong built-in bridge subnet",
        )?;
        require(
            bridge["IPAM"]["Config"][0]["Gateway"] == "172.17.0.1",
            "Docker list exposed the wrong built-in bridge gateway",
        )?;

        for name in ["bridge", "none"] {
            let response = exchange(
                &socket,
                format!(
                    "DELETE /v1.43/networks/{name}?force=true HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await?;
            require(
                response.starts_with("HTTP/1.1 403"),
                "built-in network removal did not return HTTP 403",
            )?;
            require(
                body(&response)?["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("predefined")),
                "built-in network removal omitted Docker error detail",
            )?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = shutdown.send(());
    server.await??;
    result
}
