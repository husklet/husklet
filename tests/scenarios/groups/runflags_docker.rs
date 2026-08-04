//! Docker-owned run flag contracts driven through the typed client.

use hl_client::{
    Client,
    model::{CreateContainer, ExposedPorts, HostConfig, List, PortBinding, PortBindings},
};
use hl_container::Containers;
use hl_daemon::Daemon;
use hl_images::{
    Digest, Reference,
    format::docker::{Archive, Limits},
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Duration};
use tempfile::TempDir;
use tokio::sync::oneshot;

type Error = Box<dyn std::error::Error>;
const IMAGE: &str = "alpine:3.20";

pub(super) async fn run(id: &str, containers: &Containers, rootfs: &Path, work: &Path) -> Result<(), Error> {
    let reference: Reference = IMAGE.parse()?;
    if containers.images()?.resolve(&reference)?.is_none() {
        seed(containers, rootfs)?;
    }
    let runtime = TempDir::new_in(work)?;
    let socket = runtime.path().join("daemon.sock");
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
    let result = match id {
        "runflags/publish-p" => publish(&client).await,
        "runflags/rm" => auto_remove(&client).await,
        "runflags/user-name" => named_user(&client).await,
        "runflags/network-bridge" => bridge(&client).await,
        "runflags/env-e" => environment(&client).await,
        _ => unreachable!(),
    };
    cleanup(&client).await;
    let _ = shutdown.send(());
    let stopped = server.await?;
    result?;
    stopped?;
    Ok(())
}

fn seed(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    let mut layer = tar::Builder::new(Vec::new());
    layer.follow_symlinks(false);
    let mut entries = std::fs::read_dir(rootfs)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if entry.file_type()?.is_dir() {
            layer.append_dir_all(entry.file_name(), entry.path())?;
        } else {
            layer.append_path_with_name(entry.path(), entry.file_name())?;
        }
    }
    let layer = layer.into_inner()?;
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {"Cmd": ["/bin/sh"], "WorkingDir": "/"},
        "rootfs": {"type": "layers", "diff_ids": [Digest::sha256(&layer).to_string()]}
    }))?;
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json",
        "RepoTags": [IMAGE],
        "Layers": ["layer.tar"]
    }]))?;
    let mut archive = tar::Builder::new(Vec::new());
    append(&mut archive, "config.json", &config)?;
    append(&mut archive, "layer.tar", &layer)?;
    append(&mut archive, "manifest.json", &manifest)?;
    Archive::load(&archive.into_inner()?[..], &containers.images()?, Limits::default())?;
    Ok(())
}

fn append(archive: &mut tar::Builder<Vec<u8>>, path: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes)
}

async fn publish(client: &Client) -> Result<(), Error> {
    let mut exposed = BTreeMap::new();
    exposed.insert("8080/tcp".into(), json!({}));
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "8080/tcp".into(),
        Some(vec![PortBinding {
            host_ip: "127.0.0.1".into(),
            host_port: String::new(),
        }]),
    );
    let request = request(["/bin/sleep", "30"]);
    let request = CreateContainer {
        exposed_ports: ExposedPorts(exposed),
        host_config: Some(HostConfig {
            port_bindings: PortBindings(bindings),
            ..HostConfig::default()
        }),
        ..request
    };
    client.containers().create(&request, Some("rf-auto-port")).await?;
    client.containers().start("rf-auto-port").await?;
    let inspect = client.containers().inspect("rf-auto-port").await?;
    let binding = inspect
        .network_settings
        .ports
        .get("8080/tcp")
        .and_then(|values| values.as_ref())
        .and_then(|values| values.first())
        .ok_or("inspect omitted automatic port binding")?;
    require(
        binding.host_ip == "127.0.0.1" && binding.host_port.parse::<u16>()? != 0,
        "invalid automatic binding",
    )
}

async fn auto_remove(client: &Client) -> Result<(), Error> {
    let mut request = request(["/bin/true"]);
    request.host_config = Some(HostConfig {
        auto_remove: true,
        ..HostConfig::default()
    });
    client.containers().create(&request, Some("rf-auto-remove")).await?;
    client.containers().start("rf-auto-remove").await?;
    client.containers().wait("rf-auto-remove").await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if client.containers().inspect("rf-auto-remove").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    Ok(())
}

async fn named_user(client: &Client) -> Result<(), Error> {
    let mut request = request(["/usr/bin/id", "-u"]);
    request.user = Some("nobody".into());
    completed(client, "rf-named-user", request, "65534").await
}

async fn bridge(client: &Client) -> Result<(), Error> {
    let request = request(["/bin/sleep", "30"]);
    client.containers().create(&request, Some("rf-bridge")).await?;
    client.containers().start("rf-bridge").await?;
    let network = client.networks().inspect("bridge").await?;
    require(
        network
            .containers
            .values()
            .any(|container| container.name == "rf-bridge"),
        "default bridge missing",
    )
}

async fn environment(client: &Client) -> Result<(), Error> {
    let mut request = request(["/usr/bin/env"]);
    request.env = Some(vec!["FOO=barbaz".into()]);
    completed(client, "rf-env", request, "FOO=barbaz").await
}

async fn completed(client: &Client, name: &str, request: CreateContainer, marker: &str) -> Result<(), Error> {
    client.containers().create(&request, Some(name)).await?;
    client.containers().start(name).await?;
    let wait = client.containers().wait(name).await?;
    let logs = client.containers().logs(name, true, true).await?;
    require(
        wait.status_code == 0 && String::from_utf8_lossy(&logs.stdout).contains(marker),
        "exit/output mismatch",
    )
}

fn request<const N: usize>(arguments: [&str; N]) -> CreateContainer {
    CreateContainer {
        image: IMAGE.into(),
        cmd: Some(arguments.into_iter().map(str::to_owned).collect()),
        ..CreateContainer::default()
    }
}

async fn cleanup(client: &Client) {
    if let Ok(containers) = client.containers().list(List::default().all()).await {
        for container in containers {
            let _ = client.containers().remove(&container.metadata.id, true, true).await;
        }
    }
}

async fn wait(socket: &Path) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

fn require(value: bool, message: &'static str) -> Result<(), Error> {
    if value { Ok(()) } else { Err(message.into()) }
}
