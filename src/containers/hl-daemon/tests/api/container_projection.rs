//! Raw Docker create, list, and inspect projection of durable process and lifecycle state.

use crate::api::support::{raw_http, require, wait_for_path, write_named_image_archive};
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use hl_images::{
    Images,
    format::docker::{Archive, Limits},
};
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let archive = work.path().join("process.tar");
    write_named_image_archive(&archive, "scenario/process:v1", b"fixture\n")?;
    let images = Images::open(work.path().join("images"))?;
    Archive::load(std::fs::File::open(&archive)?, &images, Limits::default())?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .images(images)
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
        "Image": "scenario/process:v1",
        "Entrypoint": ["/bin/process"],
        "Cmd": ["alpha", "two words"],
        "Labels": {"contract": "projection"}
    });
    let created = exchange(
        socket,
        "POST",
        "/v1.43/containers/create?name=truthful-process",
        Some(serde_json::to_vec(&request)?),
    )
    .await?;
    require(
        created.0.starts_with("HTTP/1.1 201"),
        "container create was not HTTP 201",
    )?;
    let id = created.1["Id"].as_str().ok_or("container create omitted Id")?;

    let listed = exchange(socket, "GET", "/v1.43/containers/json?all=true", None).await?;
    require(listed.0.starts_with("HTTP/1.1 200"), "container list was not HTTP 200")?;
    let summaries = listed.1.as_array().ok_or("container list was not an array")?;
    require(
        summaries.len() == 1,
        "container list did not return the created container",
    )?;
    let summary = &summaries[0];
    require(summary["Id"] == id, "container list changed Id")?;
    require(
        summary["Names"] == json!(["/truthful-process"]),
        "container list changed Names",
    )?;
    require(
        summary["Command"] == "/bin/process alpha two words",
        "container list changed effective Command",
    )?;
    require(
        summary["State"] == "created",
        "container list fabricated lifecycle State",
    )?;
    require(
        summary["Status"] == "Created",
        "container list fabricated lifecycle Status",
    )?;

    let inspected = exchange(socket, "GET", &format!("/v1.43/containers/{id}/json"), None).await?;
    require(
        inspected.0.starts_with("HTTP/1.1 200"),
        "container inspect was not HTTP 200",
    )?;
    require(inspected.1["Id"] == id, "container inspect changed Id")?;
    require(
        inspected.1["Name"] == "/truthful-process",
        "container inspect changed Name",
    )?;
    require(
        inspected.1["Path"] == "/bin/process",
        "container inspect omitted effective Path",
    )?;
    require(
        inspected.1["Args"] == json!(["alpha", "two words"]),
        "container inspect omitted effective Args",
    )?;
    let state = &inspected.1["State"];
    require(state["Status"] == "created", "inspect State.Status was not created")?;
    require(state["Running"] == false, "inspect State.Running was fabricated")?;
    require(state["Paused"] == false, "inspect State.Paused was fabricated")?;
    require(state["Restarting"] == false, "inspect State.Restarting was fabricated")?;
    require(state["Pid"] == 0, "inspect State.Pid was fabricated")?;
    require(state["ExitCode"] == 0, "inspect State.ExitCode was fabricated")
}

async fn remove(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let response = exchange(socket, "DELETE", "/v1.43/containers/truthful-process?force=true", None).await?;
    require(
        response.0.starts_with("HTTP/1.1 204"),
        "container cleanup was not HTTP 204",
    )
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
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("container response omitted body separator")?;
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body)?
    };
    Ok((head.to_owned(), value))
}
