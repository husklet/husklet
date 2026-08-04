use hl_client::{Client, Config as ClientConfig};
use hl_container::{Config, ContainerSpec, Containers, NetworkSpec, Process, Subnet, VolumeSpec};
use hl_daemon::Daemon;
use hl_images::{Platform, Reference, RuntimeConfig};
use std::{net::Ipv4Addr, path::Path, time::Duration};
use tempfile::TempDir;
use tokio::sync::oneshot;

struct Fixture {
    _root: TempDir,
    containers: Containers,
    client: Client,
    stop: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<hl_daemon::Result<()>>,
}

impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let containers = Containers::builder(Config::new(root.path().join("state")))
            .build()
            .await
            .unwrap();
        let socket = root.path().join("daemon.sock");
        let (stop, stopped) = oneshot::channel();
        let server = tokio::spawn(
            Daemon::new(containers.clone())
                .server(&socket)
                .serve_with_shutdown(async move {
                    let _ = stopped.await;
                }),
        );
        Self::ready(&socket).await;
        let client = Client::with_config(ClientConfig::unix(&socket)).unwrap();
        Self {
            _root: root,
            containers,
            client,
            stop,
            server,
        }
    }

    async fn ready(socket: &Path) {
        for _ in 0..100 {
            if socket.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("daemon socket did not become ready");
    }

    async fn finish(self) {
        self.stop.send(()).unwrap();
        self.server.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn system_prune_reclaims_unused_resources_and_respects_volume_selection() {
    let fixture = Fixture::new().await;
    let container = fixture
        .containers
        .create(ContainerSpec::from_directory("/rootfs", Process::new("/bin/true")).name("stopped"))
        .await
        .unwrap();
    let network = fixture
        .containers
        .networks()
        .create(NetworkSpec::bridge(
            "unused",
            Subnet::new(Ipv4Addr::new(10, 91, 0, 0), 24).unwrap(),
        ))
        .await
        .unwrap();
    let volume = fixture
        .containers
        .volumes()
        .create(VolumeSpec::new("unused-volume"))
        .await
        .unwrap();
    let mut layer = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut layer);
        let body = b"unused\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, "unused", &body[..]).unwrap();
        archive.finish().unwrap();
    }
    let images = fixture.containers.images().unwrap();
    let image_name: Reference = "system/unused:latest".parse().unwrap();
    let image = images
        .commit(
            &layer,
            &RuntimeConfig {
                entrypoint: Vec::new(),
                command: vec!["/bin/true".into()],
                environment: Default::default(),
                working_directory: "/".into(),
                user: String::new(),
            },
            &Platform::linux_arm64(),
            &image_name,
        )
        .unwrap();
    let image_digest = image.target.digest().to_string();
    images.remove(&image_name).unwrap();

    let first = fixture.client.system().prune(false).await.unwrap();
    assert_eq!(first.containers_deleted, vec![container.id.to_string()]);
    assert_eq!(first.networks_deleted, vec![network.name]);
    assert_eq!(first.images_deleted.len(), 1);
    assert_eq!(first.images_deleted[0].deleted.as_deref(), Some(image_digest.as_str()));
    assert!(first.space_reclaimed > 0);
    assert!(first.volumes_deleted.is_empty());
    assert_eq!(
        fixture.containers.volumes().inspect(&volume.name).await.unwrap().name,
        volume.name
    );

    let second = fixture.client.system().prune(true).await.unwrap();
    assert_eq!(second.volumes_deleted, vec![volume.name]);
    assert!(fixture.containers.list().await.unwrap().is_empty());
    fixture.finish().await;
}
