use hl_client::{Client, model::CreateContainer};
use hl_container::Containers;
use hl_daemon::Daemon;
use tempfile::TempDir;
use tokio::sync::oneshot;

use super::fixture;

mod advanced;
mod cache;
mod copy;
mod metadata;
mod multistage;
mod run;
mod support;

use support::append;

type Error = Box<dyn std::error::Error>;

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    let work = TempDir::new()?;
    let base = fixture::alpine(work.path())?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(
        Daemon::new(containers.clone())
            .server(&socket)
            .serve_with_shutdown(async move {
                let _ = stopped.await;
            }),
    );
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let client = Client::unix(&socket)?;
    let images = client.images();
    let api = client.containers();
    images.load(tokio::fs::File::open(base).await?).await?;
    let id = images
        .build(std::io::Cursor::new(context()?), "workflow/built:test", None)
        .await?;
    if id.is_empty() {
        return Err("build returned an empty image ID".into());
    }
    let created = api
        .create(
            &CreateContainer {
                image: "workflow/built:test".into(),
                ..CreateContainer::default()
            },
            Some("built-contract"),
        )
        .await?;
    api.start(&created.id).await?;
    let status = api.wait(&created.id).await?;
    let logs = api.logs(&created.id, true, true).await?;
    let expected = b"BUILT_HELLO\nlayerdata\nCOPIEDPAYLOAD\n/app\n";
    if status.status_code != 0 || logs.stdout != expected {
        return Err(format!(
            "built image mismatch: status={} stdout={:?} stderr={:?}",
            status.status_code, logs.stdout, logs.stderr
        )
        .into());
    }
    api.remove(&created.id, false, false).await?;

    let simple = images
        .build(std::io::Cursor::new(simple_context()?), "workflow/simple:test", None)
        .await?;
    if simple.is_empty() {
        return Err("simple build returned an empty image ID".into());
    }
    let created = api
        .create(
            &CreateContainer {
                image: "workflow/simple:test".into(),
                ..CreateContainer::default()
            },
            Some("simple-contract"),
        )
        .await?;
    api.start(&created.id).await?;
    let status = api.wait(&created.id).await?;
    let logs = api.logs(&created.id, true, true).await?;
    if status.status_code != 0 || logs.stdout != b"SIMPLEBUILT\n" {
        return Err(format!(
            "simple built image mismatch: status={} stdout={:?} stderr={:?}",
            status.status_code, logs.stdout, logs.stderr
        )
        .into());
    }
    api.remove(&created.id, false, false).await?;

    advanced::advanced(&client).await?;
    multistage::multistage(&client).await?;
    copy::external_image_copy(&client).await?;
    run::automatic_platform(&client).await?;
    run::run_mounts(&client).await?;
    copy::named_ownership(&client).await?;
    copy::modern_copy(&client).await?;
    metadata::metadata(&client).await?;
    cache::cache(&client).await?;
    copy::ownership(&client).await?;
    advanced::invalid(&client).await?;
    let _ = shutdown.send(());
    server.await??;
    println!("PASS docker-build");
    Ok(())
}

fn simple_context() -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        append(
            &mut archive,
            "Dockerfile",
            b"FROM workflow/alpine:test\nCMD echo SIMPLEBUILT\n",
        )?;
        archive.finish()?;
    }
    Ok(bytes)
}

fn context() -> Result<Vec<u8>, Error> {
    let dockerfile = b"FROM workflow/alpine:test\nENV GREETING=BUILT_HELLO\nRUN test ! -e /payload.txt && echo layerdata > /layerfile\nCOPY payload.txt /payload.txt\nWORKDIR /app\nCMD sh -c \"echo $GREETING; cat /layerfile; cat /payload.txt; pwd\"\n";
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        append(&mut archive, "Dockerfile", dockerfile)?;
        append(&mut archive, "payload.txt", b"COPIEDPAYLOAD\n")?;
        archive.finish()?;
    }
    Ok(bytes)
}
