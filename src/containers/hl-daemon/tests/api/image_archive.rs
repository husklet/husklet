//! Image archive load/list/inspect/tag/save/remove round trip.

use crate::api::support::{raw_http, require, wait_for_path, write_image_archive};
use hl_client::Client;
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use std::io::Cursor;
use tempfile::TempDir;
use tokio::sync::oneshot;

// Keep the whole protocol journey visible in order: splitting it obscures which durable resource a
// later assertion is proving came from an earlier request.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let archive = work.path().join("fixture.tar");
    write_image_archive(&archive)?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;
    let loaded = client.images().load(tokio::fs::File::open(&archive).await?).await?;
    require(
        loaded.stream == "Loaded image: docker.io/scenario/fixture:test\n",
        &format!("image load did not report the imported tag: {:?}", loaded.stream),
    )?;
    let listed = client.images().list().await?;
    require(listed.len() == 1, "image load did not create exactly one image")?;
    require(
        listed[0].repo_tags == ["docker.io/scenario/fixture:test"],
        "image list did not preserve the imported tag",
    )?;
    let filtered = raw_http(
        &socket,
        b"GET /v1.43/images/json?filters=%7B%22reference%22%3A%7B%22docker.io%2Fscenario%2Ffixture%3Atest%22%3Atrue%7D%7D HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    require(
        filtered.starts_with("HTTP/1.1 200") && filtered.contains("docker.io/scenario/fixture:test"),
        "Docker map-set image filter did not select the loaded image",
    )?;
    let normalized: hl_images::Reference = "scenario/fixture:test".parse()?;
    require(
        normalized.to_string() == "docker.io/scenario/fixture:test",
        "short image reference normalized unexpectedly",
    )?;
    for target in [
        "/v1.43/distribution/scenario%2Ffixture%3Atest/json",
        "/v1.43/distribution/docker.io/scenario/fixture:test/json",
    ] {
        let response = raw_http(&socket, format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes())
            .await?;
        require(response.starts_with("HTTP/1.1 200"), "distribution reference was not resolved")?;
    }
    let id_prefix = listed[0].id.strip_prefix("sha256:").unwrap_or(&listed[0].id)[..12].to_owned();
    for identity in [&listed[0].id, &id_prefix] {
        let target = format!("/v1.43/distribution/{identity}/json");
        let response = raw_http(
            &socket,
            format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
        require(response.starts_with("HTTP/1.1 200"), "distribution image ID was not resolved")?;
    }
    for (method, target) in [
        ("POST", "/v1.43/distribution/docker.io/scenario/fixture:test/json"),
        ("GET", "/v1.43/distribution/docker.io/scenario/fixture:test/history"),
        ("GET", "/v1.43/distribution/docker.io/scenario/missing:test/json"),
    ] {
        let response = raw_http(
            &socket,
            format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .await?;
        require(response.starts_with("HTTP/1.1 404"), "distribution route isolation changed")?;
    }
    let inspected = client
        .images()
        .inspect("scenario/fixture:test")
        .await
        .map_err(|error| format!("image inspect: {error}"))?;
    require(
        inspected.id == listed[0].id,
        "image inspect and list disagreed on identity",
    )?;
    client
        .images()
        .tag("scenario/fixture:test", "scenario/copy", Some("v1"))
        .await
        .map_err(|error| format!("image tag: {error}"))?;
    require(
        client
            .images()
            .inspect("scenario/copy:v1")
            .await
            .map_err(|error| format!("tagged image inspect: {error}"))?
            .id
            == inspected.id,
        "image tag changed immutable identity",
    )?;

    let created = client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/fixture:test".into(),
                cmd: Some(vec!["/bin/true".into()]),
                ..Default::default()
            },
            Some("from-image"),
        )
        .await
        .map_err(|error| format!("image-backed container create: {error}"))?;
    let container = client.containers().inspect(&created.id).await?;
    require(
        container.metadata.image == "docker.io/scenario/fixture:test",
        "image-backed create lost image identity",
    )?;
    client.containers().remove(&created.id, false, false).await?;

    let mut saved = client
        .images()
        .save(&["scenario/fixture:test"])
        .await
        .map_err(|error| format!("image save: {error}"))?;
    let mut archive_bytes = Vec::new();
    while let Some(chunk) = saved.next_chunk().await? {
        archive_bytes.extend_from_slice(&chunk);
        require(
            archive_bytes.len() <= 4 * 1024 * 1024,
            "tiny image save unexpectedly exceeded four MiB",
        )?;
    }
    let mut names = Vec::new();
    for item in tar::Archive::new(Cursor::new(archive_bytes)).entries()? {
        names.push(item?.path()?.to_string_lossy().into_owned());
    }
    require(
        names.iter().any(|name| name == "manifest.json"),
        "saved Docker archive omitted manifest.json",
    )?;

    client
        .images()
        .remove("scenario/copy:v1")
        .await
        .map_err(|error| format!("tagged image remove: {error}"))?;
    client
        .images()
        .remove("scenario/fixture:test")
        .await
        .map_err(|error| format!("source image remove: {error}"))?;
    require(client.images().list().await?.is_empty(), "image removal left tags")?;
    let _ = shutdown.send(());
    server.await??;
    println!("PASS image-archive-create");
    Ok(())
}
