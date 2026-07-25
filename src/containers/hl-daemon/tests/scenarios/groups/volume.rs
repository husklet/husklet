//! Exact bind and managed-volume scenarios through the typed Docker client.

use crate::report::ScenarioBatch;
use hl_client::Client;
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use sha2::{Digest, Sha256};
use std::{env, fmt::Write as _, io::Read, path::Path};
use tempfile::TempDir;
use tokio::sync::oneshot;

mod bind;
mod execution;
mod managed;

type Error = Box<dyn std::error::Error>;
pub(crate) const IMAGE: &str = "contract/alpine:test";

pub(crate) async fn run() -> Result<(), Error> {
    let work = TempDir::new()?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let archive = archive(work.path())?;
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
    client
        .images()
        .load(tokio::fs::File::open(archive).await?)
        .await?;
    let result = async {
        managed::run_docker_contracts(&client).await?;
        let scenarios = crate::registry::volume::group()
            .scenarios
            .into_iter()
            .map(|value| (value.id, value))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut reports = ScenarioBatch::new("volumes")?;
        let result = bind::binds(&client, &scenarios, &mut reports).await;
        reports.finish(env::var("HL_VOLUME_CASE").ok().into_iter().collect())?;
        result
    }
    .await;
    let _ = shutdown.send(());
    server.await??;
    result
}

async fn wait(path: &Path) -> Result<(), Error> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

pub(crate) fn archive(work: &Path) -> Result<std::path::PathBuf, Error> {
    let source = env::var_os("HL_ALPINE_ARCHIVE").ok_or("HL_ALPINE_ARCHIVE is required")?;
    let mut layer = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&source)?).read_to_end(&mut layer)?;
    let digest = Sha256::digest(&layer)
        .iter()
        .fold(String::new(), |mut digest, byte| {
            write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
            digest
        });
    let config = serde_json::to_vec(
        &serde_json::json!({"architecture":"arm64","os":"linux","config":{"Cmd":["/bin/sh"]},"rootfs":{"type":"layers","diff_ids":[format!("sha256:{digest}")]}}),
    )?;
    let manifest = serde_json::to_vec(
        &serde_json::json!([{"Config":"config.json","RepoTags":[IMAGE],"Layers":["layer.tar"]}]),
    )?;
    let path = work.join("alpine.tar");
    let mut tar = tar::Builder::new(std::fs::File::create(&path)?);
    for (name, bytes) in [
        ("config.json", config.as_slice()),
        ("layer.tar", layer.as_slice()),
        ("manifest.json", manifest.as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes)?;
    }
    tar.finish()?;
    Ok(path)
}
