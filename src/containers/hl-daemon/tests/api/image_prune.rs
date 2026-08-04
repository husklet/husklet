//! Image prune preserves graphs referenced by containers while removing other named graphs.

use crate::api::support::{raw_http, require, wait_for_path, write_named_image_archive};
use hl_client::Client;
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use hl_images::{
    Images,
    format::docker::{Archive, Limits},
};
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let unused_archive = work.path().join("unused.tar");
    let used_archive = work.path().join("used.tar");
    let external_archive = work.path().join("external.tar");
    let shared_external_archive = work.path().join("shared-external.tar");
    let shared_local_archive = work.path().join("shared-local.tar");
    write_named_image_archive(&unused_archive, "scenario/unused:v1", b"unused\n")?;
    write_named_image_archive(&used_archive, "scenario/used:v1", b"used\n")?;
    write_named_image_archive(&external_archive, "scenario/external:v1", b"external\n")?;
    write_named_image_archive(&shared_external_archive, "scenario/shared:external", b"shared\n")?;
    write_named_image_archive(&shared_local_archive, "scenario/shared:local", b"shared\n")?;

    let local = Images::open(work.path().join("local-images"))?;
    let external = Images::open(work.path().join("external-images"))?;
    Archive::load(std::fs::File::open(&external_archive)?, &external, Limits::default())?;
    Archive::load(
        std::fs::File::open(&shared_external_archive)?,
        &external,
        Limits::default(),
    )?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .images(Images::workspace(local, external.clone()))
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;
    client
        .images()
        .load(tokio::fs::File::open(&unused_archive).await?)
        .await?;
    client
        .images()
        .load(tokio::fs::File::open(&used_archive).await?)
        .await?;
    client
        .images()
        .load(tokio::fs::File::open(&shared_local_archive).await?)
        .await?;
    client
        .containers()
        .create(
            &hl_client::model::CreateContainer {
                image: "scenario/used:v1".into(),
                cmd: Some(vec!["/bin/true".into()]),
                ..Default::default()
            },
            Some("image-prune-reference"),
        )
        .await?;

    let response = raw_http(
        &socket,
        b"POST /v1.43/images/prune?filters=%7B%22dangling%22%3A%5B%22false%22%5D%7D HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await?;
    require(response.starts_with("HTTP/1.1 200"), "all-unused image prune failed")?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .ok_or("prune response omitted HTTP body")?;
    let report: hl_client::model::ImagePrune = serde_json::from_str(body)?;
    require(
        report.images_deleted.len() == 3,
        "prune response omitted image mutations",
    )?;
    require(report.space_reclaimed > 0, "prune response omitted reclaimed bytes")?;
    require(
        report
            .images_deleted
            .iter()
            .any(|entry| entry.untagged.as_deref() == Some("docker.io/scenario/unused:v1") && entry.deleted.is_none()),
        "prune response omitted removed tag",
    )?;
    require(
        report
            .images_deleted
            .iter()
            .filter(|entry| {
                entry
                    .deleted
                    .as_deref()
                    .is_some_and(|digest| digest.starts_with("sha256:"))
            })
            .count()
            == 1,
        "prune response omitted deleted target",
    )?;
    require(
        report.images_deleted.iter().any(|entry| {
            entry.untagged.as_deref() == Some("docker.io/scenario/shared:local") && entry.deleted.is_none()
        }),
        "prune response falsely deleted the shared external target",
    )?;
    require(
        report.images_deleted.iter().all(|entry| {
            entry.untagged.as_deref() != Some("docker.io/scenario/external:v1")
                && entry.untagged.as_deref() != Some("docker.io/scenario/shared:external")
        }),
        "prune response synthesized an external-catalog mutation",
    )?;

    let listed = client.images().list().await?;
    require(listed.len() == 3, "all-unused prune did not preserve external graphs")?;
    require(
        listed
            .iter()
            .any(|image| image.repo_tags == ["docker.io/scenario/used:v1"]),
        "all-unused prune removed a container-referenced graph",
    )?;
    require(
        listed
            .iter()
            .any(|image| image.repo_tags == ["docker.io/scenario/external:v1"]),
        "all-unused prune removed the external-only graph",
    )?;
    require(
        listed
            .iter()
            .any(|image| image.repo_tags == ["docker.io/scenario/shared:external"]),
        "all-unused prune removed the shared external alias",
    )?;

    let _ = shutdown.send(());
    server.await??;
    Ok(())
}
