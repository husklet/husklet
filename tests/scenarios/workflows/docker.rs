//! Retained root-filesystem import coverage with no package-owned replacement yet.

use hl_client::{Client, model::CreateContainer};
use hl_container::Containers;
use hl_daemon::Daemon;
use tempfile::TempDir;
use tokio::sync::oneshot;

use super::fixture;

type Error = Box<dyn std::error::Error>;

pub(super) async fn run(containers: &Containers) -> Result<(), Error> {
    let work = TempDir::new()?;
    let archive = fixture::alpine(work.path())?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(
        Daemon::new(containers.clone())
            .server(&socket)
            .serve_with_shutdown(async move {
                let _ = stopped.await;
            }),
    );
    wait(&socket).await?;
    let client = Client::unix(&socket)?;
    client.images().load(tokio::fs::File::open(archive).await?).await?;
    import_rootfs(&client).await?;
    let _ = shutdown.send(());
    server.await??;
    if !containers.list().await?.is_empty() {
        return Err("docker import workflow leaked container records".into());
    }
    Ok(())
}

async fn import_rootfs(client: &Client) -> Result<(), Error> {
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: fixture::IMAGE.into(),
                cmd: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf imported > /imported.txt".into(),
                ]),
                ..CreateContainer::default()
            },
            Some("workflow-import"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    if client.containers().wait(&created.id).await?.status_code != 0 {
        return Err("rootfs import source process failed".into());
    }
    let exported = collect(client.containers().export(&created.id).await?).await?;
    let imported = client
        .images()
        .import(std::io::Cursor::new(exported), "workflow/imported", Some("test"))
        .await?;
    if !imported.stream.contains("workflow/imported:test")
        || client.images().inspect("workflow/imported:test").await?.id.is_empty()
    {
        return Err("rootfs import was not discoverable by its requested tag".into());
    }
    client.containers().remove(&created.id, false, false).await?;
    Ok(())
}

async fn collect(mut stream: hl_client::Stream) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next_chunk().await? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn wait(socket: &std::path::Path) -> Result<(), Error> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}
