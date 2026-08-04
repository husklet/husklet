//! Image prune preserves graphs referenced by containers while removing other named graphs.

use crate::api::support::{raw_http, require, wait_for_path, write_named_image_archive};
use hl_client::Client;
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let unused_archive = work.path().join("unused.tar");
    let used_archive = work.path().join("used.tar");
    write_named_image_archive(&unused_archive, "scenario/unused:v1", b"unused\n")?;
    write_named_image_archive(&used_archive, "scenario/used:v1", b"used\n")?;

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
    client
        .images()
        .load(tokio::fs::File::open(&unused_archive).await?)
        .await?;
    client
        .images()
        .load(tokio::fs::File::open(&used_archive).await?)
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
    require(
        response.contains("docker.io/scenario/unused:v1"),
        "prune response omitted removed tag",
    )?;

    let listed = client.images().list().await?;
    require(listed.len() == 1, "all-unused prune did not remove exactly one graph")?;
    require(
        listed[0].repo_tags == ["docker.io/scenario/used:v1"],
        "all-unused prune removed a container-referenced graph",
    )?;

    let _ = shutdown.send(());
    server.await??;
    Ok(())
}
