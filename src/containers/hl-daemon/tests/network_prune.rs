//! Docker network-prune filtering over the raw daemon wire.

use hl_container::{Config, Containers, NetworkSpec};
use hl_daemon::Daemon;
use serde_json::Value;
use std::{collections::BTreeSet, path::Path, time::Duration};
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
            return Err("network prune response exceeded one MiB".into());
        }
        String::from_utf8(response).map_err(Into::into)
    })
    .await
    .map_err(|_| "network prune exchange timed out")?
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

async fn exchange(socket: &Path, filters: &str) -> Result<String, Box<dyn std::error::Error>> {
    let encoded = filters.bytes().map(|byte| format!("%{byte:02X}")).collect::<String>();
    let request = format!(
        "POST /v1.43/networks/prune?filters={encoded} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    raw_http(socket, request.as_bytes()).await
}

fn deleted(response: &str) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    if !response.starts_with("HTTP/1.1 200") {
        return Err("network prune did not return HTTP 200".into());
    }
    let body = response
        .split_once("\r\n\r\n")
        .ok_or("network prune response omitted its body")?
        .1;
    let value: Value = serde_json::from_str(body)?;
    value["NetworksDeleted"]
        .as_array()
        .ok_or_else(|| -> Box<dyn std::error::Error> { "network prune response omitted NetworksDeleted".into() })?
        .iter()
        .map(|name| {
            name.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "network prune returned a non-string name".into())
        })
        .collect()
}

#[tokio::test]
async fn wire_contract() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let networks = containers.networks();
    networks
        .create(
            NetworkSpec::none("protected")
                .label("owner", "protected")
                .label("stage", "prod"),
        )
        .await?;
    networks
        .create(NetworkSpec::none("one-match").label("owner", "protected"))
        .await?;
    networks.create(NetworkSpec::none("no-match")).await?;

    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let result = async {
        for invalid in [
            r#"{"driver":["none"]}"#,
            r#"{"until":["invalid"]}"#,
            r#"{"until":["1","2"]}"#,
        ] {
            let response = exchange(&socket, invalid).await?;
            if !response.starts_with("HTTP/1.1 400") {
                return Err("invalid network-prune filter was not HTTP 400".into());
            }
        }

        let response = exchange(&socket, r#"{"label!":{"owner=protected":false,"stage=prod":true}}"#).await?;
        let expected = ["no-match".to_owned(), "one-match".to_owned()].into_iter().collect();
        if deleted(&response)? != expected {
            return Err("negated-label prune did not preserve Moby's set semantics".into());
        }

        networks
            .create(
                NetworkSpec::none("positive-full")
                    .label("owner", "team")
                    .label("stage", "prod"),
            )
            .await?;
        networks
            .create(NetworkSpec::none("positive-partial").label("owner", "team"))
            .await?;
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
            .saturating_add(3_600);
        let response = exchange(
            &socket,
            &format!(r#"{{"label":["owner=team","stage=prod"],"until":["{future}"]}}"#),
        )
        .await?;
        if deleted(&response)? != ["positive-full".to_owned()].into_iter().collect() {
            return Err("positive-label prune did not require every label".into());
        }

        let remaining = networks
            .list()
            .await?
            .into_iter()
            .map(|network| network.name)
            .collect::<BTreeSet<_>>();
        let expected = ["bridge", "none", "positive-partial", "protected"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        if remaining != expected {
            return Err("network prune changed protected or predefined networks".into());
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = shutdown.send(());
    server.await??;
    result
}
