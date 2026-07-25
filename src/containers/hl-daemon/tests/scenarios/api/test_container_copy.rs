//! Container filesystem copy-to and copy-from over the daemon socket.

use crate::api::support::{append_archive_member, require, wait_for_path};
use hl_client::Client;
use hl_container::{Config, ContainerSpec, Containers, Process};
use hl_daemon::Daemon;
use std::io::Cursor;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = work.path().join("rootfs");
    std::fs::create_dir_all(rootfs.join("inbox"))?;
    std::fs::write(rootfs.join("outbox"), b"downloaded")?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let container = containers
        .create(ContainerSpec::from_directory(
            &rootfs,
            Process::new("/bin/true"),
        ))
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(
        async move {
            let _ = stopped.await;
        },
    ));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;

    let mut upload = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut upload);
        append_archive_member(&mut archive, "nested/uploaded", b"uploaded")?;
        archive.finish()?;
    }
    client
        .containers()
        .copy_to(container.id.as_str(), "/inbox", Cursor::new(upload))
        .await?;
    require(
        std::fs::read(rootfs.join("inbox/nested/uploaded"))? == b"uploaded",
        "copy-to did not update the selected container rootfs",
    )?;

    let copied = client
        .containers()
        .copy_from(container.id.as_str(), "/outbox")
        .await?;
    require(
        copied.stat().name == "outbox" && copied.stat().size == 10,
        "copy-from metadata did not describe the selected file",
    )?;
    let mut stream = copied.into_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next_chunk().await? {
        bytes.extend_from_slice(&chunk);
    }
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut entries = archive.entries()?;
    let mut entry = entries.next().ok_or("copy-from archive was empty")??;
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut payload)?;
    require(
        payload == b"downloaded",
        "copy-from returned wrong contents",
    )?;

    let _ = shutdown.send(());
    server.await??;
    println!("PASS container-copy");
    Ok(())
}
