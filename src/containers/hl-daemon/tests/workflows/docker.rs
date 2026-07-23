//! Typed daemon/client API sweep replacing Docker CLI shell orchestration.

mod container;

use hl_client::{
    model::{CreateContainer, Credentials, NetworkCreate, VolumeCreate},
    Client,
};
use hl_container::Containers;
use hl_daemon::Daemon;
use tempfile::TempDir;
use tokio::sync::oneshot;

use super::fixture;

type Error = Box<dyn std::error::Error>;

pub(super) async fn run(containers: &Containers, full: bool) -> Result<(), Error> {
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
    let outcome = exercise(&client, &archive, full).await;
    let _ = shutdown.send(());
    let server = server.await?;
    outcome?;
    server?;
    if !containers.list().await?.is_empty() {
        return Err("docker workflow leaked container records".into());
    }
    Ok(())
}

async fn exercise(client: &Client, archive: &std::path::Path, full: bool) -> Result<(), Error> {
    client.ping().await?;
    pass("ping");
    let version = client.version().await?;
    require(
        !version.version.is_empty() && version.os == "linux",
        "version",
    )?;
    let info = client.system().info().await?;
    require(
        info.os_type == "linux" && !info.server_version.is_empty(),
        "info",
    )?;
    let usage = client.system().disk_usage().await?;
    require(usage.containers.is_empty(), "system-df")?;

    let loaded = client
        .images()
        .load(tokio::fs::File::open(archive).await?)
        .await?;
    require(loaded.stream.contains(fixture::IMAGE), "image-import")?;
    let image = client.images().inspect(fixture::IMAGE).await?;
    require(
        client
            .images()
            .list()
            .await?
            .iter()
            .any(|candidate| candidate.id == image.id),
        "image-discovery",
    )?;
    container::foreground(client).await?;
    container::interactive(client).await?;

    Resources::new(client).verify().await?;

    if full {
        images(client).await?;
        container::lifecycle(client).await?;
        require(client.system().plugins().await?.is_empty(), "plugins-empty")?;
        let authentication = client
            .system()
            .authenticate(&Credentials {
                username: "workflow".into(),
                password: "secret".into(),
                serveraddress: "registry.example.test".into(),
                ..Credentials::default()
            })
            .await?;
        require(authentication.status == "Login Succeeded", "registry-auth")?;
        require(
            client.images().search("alpine", Some(5)).await?.is_empty(),
            "registry-search-empty",
        )?;
        let _ = client.volumes().prune().await?;
        let _ = client.networks().prune().await?;
        let _ = client.images().prune().await?;
        let _ = client.containers().prune().await?;
        let _ = client.system().prune(true).await?;
        pass("prune-verbs");
    }
    Ok(())
}

struct Resources<'a> {
    client: &'a Client,
}

impl<'a> Resources<'a> {
    fn new(client: &'a Client) -> Self {
        Self { client }
    }

    async fn verify(&self) -> Result<(), Error> {
        self.volume().await?;
        self.network().await
    }

    async fn volume(&self) -> Result<(), Error> {
        let volumes = self.client.volumes();
        let volume = volumes
            .create(&VolumeCreate {
                name: "workflow-volume".into(),
                ..VolumeCreate::default()
            })
            .await?;
        require(
            volumes
                .list()
                .await?
                .volumes
                .iter()
                .any(|item| item.name == volume.name),
            "volume-listed",
        )?;
        volumes.remove(&volume.name, false).await?;
        require(
            !volumes
                .list()
                .await?
                .volumes
                .iter()
                .any(|item| item.name == volume.name),
            "volume-removed",
        )
    }

    async fn network(&self) -> Result<(), Error> {
        let networks = self.client.networks();
        let network = networks
            .create(&NetworkCreate {
                name: "workflow-network".into(),
                driver: "bridge".into(),
                ..NetworkCreate::default()
            })
            .await?;
        require(
            networks
                .list()
                .await?
                .iter()
                .any(|item| item.id == network.id),
            "network-listed",
        )?;
        networks.remove(&network.id, false).await?;
        require(
            !networks
                .list()
                .await?
                .iter()
                .any(|item| item.id == network.id),
            "network-removed",
        )
    }
}

async fn images(client: &Client) -> Result<(), Error> {
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: fixture::IMAGE.into(),
                cmd: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf committed > /committed.txt".into(),
                ]),
                ..CreateContainer::default()
            },
            Some("workflow-image"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    require(
        client.containers().wait(&created.id).await?.status_code == 0,
        "change-process-exit",
    )?;
    let changes = client.containers().changes(&created.id).await?;
    require(
        changes.iter().any(|change| change.path == "/committed.txt"),
        "container-changes",
    )?;
    let committed = client
        .images()
        .commit(&created.id, "workflow/committed", Some("test"), false)
        .await?;
    require(!committed.id.is_empty(), "container-commit")?;
    require(
        !client
            .images()
            .history("workflow/committed:test")
            .await?
            .is_empty(),
        "image-history",
    )?;

    let exported = collect(client.containers().export(&created.id).await?).await?;
    require(!exported.is_empty(), "container-export")?;
    let imported = client
        .images()
        .import(
            std::io::Cursor::new(exported),
            "workflow/imported",
            Some("test"),
        )
        .await?;
    require(
        imported.stream.contains("workflow/imported:test"),
        "image-import-rootfs",
    )?;
    require(
        !client
            .images()
            .inspect("workflow/imported:test")
            .await?
            .id
            .is_empty(),
        "image-import-inspect",
    )?;

    let saved = collect(client.images().save(&["workflow/committed:test"]).await?).await?;
    require(!saved.is_empty(), "image-save")?;
    client.images().remove("workflow/committed:test").await?;
    client.images().load(std::io::Cursor::new(saved)).await?;
    require(
        !client
            .images()
            .inspect("workflow/committed:test")
            .await?
            .id
            .is_empty(),
        "image-load-roundtrip",
    )?;
    client
        .containers()
        .remove(&created.id, false, false)
        .await?;
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

fn require(value: bool, name: &'static str) -> Result<(), Error> {
    if value {
        pass(name);
        Ok(())
    } else {
        Err(name.into())
    }
}

fn pass(name: &str) {
    println!("PASS docker/{name}");
}
