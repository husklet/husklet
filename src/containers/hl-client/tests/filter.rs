use hl_client::{Client, model::List};
use hl_container::{Config, ContainerSpec, Containers, Process};
use hl_daemon::Daemon;
use std::{path::Path, time::Duration};
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
        let client = Client::unix(&socket).unwrap();
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
async fn typed_filters_select_durable_container_labels_and_names() {
    let fixture = Fixture::new().await;
    for (name, role) in [("builder", "build"), ("runner", "run")] {
        fixture
            .containers
            .create(
                ContainerSpec::from_directory("/rootfs", Process::new("/bin/true"))
                    .name(name)
                    .label("role", role),
            )
            .await
            .unwrap();
    }

    let selected = fixture
        .client
        .containers()
        .list(List::default().all().label("role", "build"))
        .await
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].names, ["/builder"]);
    assert_eq!(selected[0].labels.get("role").unwrap(), "build");

    let selected = fixture
        .client
        .containers()
        .list(List::default().all().name("run"))
        .await
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].names, ["/runner"]);
    fixture.finish().await;
}
