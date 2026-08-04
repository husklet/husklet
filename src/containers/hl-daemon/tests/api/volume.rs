//! Ordinary Docker volume create, list, and inspect contracts over raw HTTP.

use crate::api::support::{raw_http, require, wait_for_path};
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let result = exercise(&socket).await;
    let _ = remove(&socket).await;
    let _ = shutdown.send(());
    server.await??;
    result
}

async fn exercise(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let request = json!({
        "Name": "raw-contract",
        "Driver": "local",
        "Labels": {"purpose": "public-wire"}
    });
    let created = exchange(
        socket,
        "POST",
        "/v1.43/volumes/create",
        Some(serde_json::to_vec(&request)?),
    )
    .await?;
    require(created.0.starts_with("HTTP/1.1 201"), "volume create was not HTTP 201")?;
    assert_volume(&created.1)?;

    let listed = exchange(socket, "GET", "/v1.43/volumes", None).await?;
    require(listed.0.starts_with("HTTP/1.1 200"), "volume list was not HTTP 200")?;
    let volumes = listed.1["Volumes"]
        .as_array()
        .ok_or("volume list omitted its Volumes array")?;
    require(volumes.len() == 1, "volume list did not contain exactly the created volume")?;
    assert_volume(&volumes[0])?;
    require(listed.1["Warnings"] == json!([]), "volume list Warnings was not empty")?;

    let inspected = exchange(socket, "GET", "/v1.43/volumes/raw-contract", None).await?;
    require(inspected.0.starts_with("HTTP/1.1 200"), "volume inspect was not HTTP 200")?;
    assert_volume(&inspected.1)?;
    require(inspected.1 == created.1, "volume inspect diverged from create")
}

fn assert_volume(volume: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let object = volume.as_object().ok_or("volume response was not an object")?;
    let keys = object.keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>();
    require(
        keys
            == ["CreatedAt", "Driver", "Labels", "Mountpoint", "Name", "Options", "Scope"]
                .into_iter()
                .collect(),
        "volume response did not use the canonical ordinary Docker shape",
    )?;
    require(volume["Name"] == "raw-contract", "volume response changed Name")?;
    require(volume["Driver"] == "local", "volume response changed Driver")?;
    require(volume["Scope"] == "local", "volume response changed Scope")?;
    require(volume["Labels"] == json!({"purpose": "public-wire"}), "volume response changed Labels")?;
    require(volume["Options"] == json!({}), "ordinary volume Options was not empty")?;
    require(
        volume["CreatedAt"].as_str().is_some_and(|value| !value.is_empty()),
        "volume response omitted CreatedAt",
    )?;
    require(
        volume["Mountpoint"]
            .as_str()
            .is_some_and(|value| value.ends_with("/volumes/raw-contract/_data")),
        "volume response omitted its owned Mountpoint",
    )
}

async fn remove(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let response = exchange(socket, "DELETE", "/v1.43/volumes/raw-contract", None).await?;
    require(response.0.starts_with("HTTP/1.1 204"), "volume cleanup was not HTTP 204")
}

async fn exchange(
    socket: &Path,
    method: &str,
    target: &str,
    body: Option<Vec<u8>>,
) -> Result<(String, Value), Box<dyn std::error::Error>> {
    let body = body.unwrap_or_default();
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut request = request.into_bytes();
    request.extend_from_slice(&body);
    let response = raw_http(socket, &request).await?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or("volume response omitted its body")?;
    let value = if body.is_empty() { Value::Null } else { serde_json::from_str(body)? };
    Ok((head.to_owned(), value))
}
