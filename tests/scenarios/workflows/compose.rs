//! Compose project topology, labels, network, volume, and teardown workflows.

use hl_container::{
    ContainerSpec, Containers, EndpointSpec, ExecSpec, ExecState, ExitStatus, Isolation, NetworkSpec, Process, Sandbox,
    Stream, Subnet, VolumeSpec,
};
use std::net::Ipv4Addr;
use std::time::Duration;
use tempfile::TempDir;

type Error = Box<dyn std::error::Error>;

pub(super) async fn run(containers: &Containers) -> Result<(), Error> {
    let roots = TempDir::new()?;
    let api = super::fixture::rootfs(roots.path(), "api")?;
    let worker = super::fixture::rootfs(roots.path(), "worker")?;
    let suffix = "resolved";
    let interpolated = format!("compose-{suffix}");
    let networks = containers.networks();
    let volumes = containers.volumes();
    networks
        .create(NetworkSpec::bridge(
            "hlcompose_appnet",
            Subnet::new(Ipv4Addr::new(172, 30, 0, 0), 24)?,
        ))
        .await?;
    volumes.create(VolumeSpec::new("hlcompose_shared")).await?;
    containers
        .create(
            ContainerSpec::from_directory(
                &api,
                Process::new("/bin/sh")
                    .args(["-c", "printf 'api-marker:%s\\n' \"$COMPOSE_VALUE\"; exec sleep 60"])
                    .env("COMPOSE_VALUE", &interpolated),
            )
            .name("hlcompose-api-1")
            .label("com.docker.compose.project", "hlcompose")
            .label("com.docker.compose.service", "api")
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                network_isolated: false,
                ..Isolation::default()
            }),
        )
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(
                &worker,
                Process::new("/bin/sh").args(["-c", "echo worker-marker; exec sleep 60"]),
            )
            .name("hlcompose-worker-1")
            .label("com.docker.compose.project", "hlcompose")
            .label("com.docker.compose.service", "worker")
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                network_isolated: false,
                ..Isolation::default()
            }),
        )
        .await?;
    networks
        .connect(
            "hlcompose_appnet",
            "hlcompose-api-1",
            EndpointSpec::default().name("api"),
        )
        .await?;
    networks
        .connect(
            "hlcompose_appnet",
            "hlcompose-worker-1",
            EndpointSpec::default().name("worker"),
        )
        .await?;
    check(
        containers.list().await?.iter().any(|container| {
            container
                .spec
                .labels
                .get("com.docker.compose.service")
                .is_some_and(|value| value == "api")
        }),
        "ps-shows-api",
    )?;
    check(
        networks
            .list()
            .await?
            .iter()
            .any(|network| network.name == "hlcompose_appnet"),
        "network-created",
    )?;
    check(
        volumes
            .list()
            .await?
            .iter()
            .any(|volume| volume.name == "hlcompose_shared"),
        "volume-created",
    )?;
    let project = Project { containers };
    project.verify().await?;
    project.remove().await
}

struct Project<'a> {
    containers: &'a Containers,
}

impl Project<'_> {
    async fn verify(&self) -> Result<(), Error> {
        self.containers.start("hlcompose-api-1").await?;
        self.containers.start("hlcompose-worker-1").await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        check(
            self.containers.inspect("hlcompose-api-1").await?.state.is_active()
                && self.containers.inspect("hlcompose-worker-1").await?.state.is_active(),
            "up-ran",
        )?;
        let api = String::from_utf8(self.containers.logs("hlcompose-api-1").await?.stdout)?;
        let worker = String::from_utf8(self.containers.logs("hlcompose-worker-1").await?.stdout)?;
        check(api.contains("api-marker:"), "logs-api-marker")?;
        check(worker.contains("worker-marker"), "logs-worker-marker")?;
        check(api.contains("api-marker:compose-resolved"), "logs-env-interp")?;
        self.execution().await
    }

    async fn execution(&self) -> Result<(), Error> {
        let executions = self.containers.executions();
        let execution = executions
            .create(
                "hlcompose-api-1",
                ExecSpec::new(Process::new("/bin/sh").args(["-c", "echo exec-api"])),
            )
            .await?;
        let mut session = executions.start(&execution.id).await?;
        let mut output = Vec::new();
        while let Some(chunk) = session.next().await? {
            if chunk.stream == Stream::Stdout {
                output.extend(chunk.bytes);
            }
        }
        let execution = executions.inspect(&execution.id).await?;
        check(
            matches!(
                execution.state,
                ExecState::Exited {
                    result: ExitStatus::Code(0),
                    ..
                }
            ) && output == b"exec-api\n",
            "exec-into-api",
        )
    }

    async fn remove(&self) -> Result<(), Error> {
        for name in ["hlcompose-api-1", "hlcompose-worker-1"] {
            self.containers.remove_force(name).await?;
        }
        let networks = self.containers.networks();
        networks.remove("hlcompose_appnet").await?;
        self.containers.volumes().remove("hlcompose_shared").await?;
        check(self.containers.list().await?.is_empty(), "down-removed-containers")?;
        check(
            !networks
                .list()
                .await?
                .iter()
                .any(|network| network.name == "hlcompose_appnet"),
            "down-removed-network",
        )
    }
}

pub(super) async fn multinet(containers: &Containers) -> Result<(), Error> {
    let root = TempDir::new()?;
    let app_root = super::fixture::rootfs(root.path(), "multinet-app")?;
    let front_root = super::fixture::rootfs(root.path(), "multinet-front")?;
    let back_root = super::fixture::rootfs(root.path(), "multinet-back")?;
    let networks = containers.networks();
    networks
        .create(NetworkSpec::bridge(
            "hlmnet_front",
            Subnet::new(Ipv4Addr::new(172, 29, 0, 0), 24)?,
        ))
        .await?;
    networks
        .create(NetworkSpec::bridge(
            "hlmnet_back",
            Subnet::new(Ipv4Addr::new(172, 29, 1, 0), 24)?,
        ))
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(&app_root, Process::new("/bin/sh").args(["-c", "exec sleep 60"]))
                .name("hlmnet-app-1")
                .isolation(networked()),
        )
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(
                &front_root,
                Process::new("/bin/sh").args(["-c", "printf 'FRONT\\n' | nc -l -p 9200"]),
            )
            .name("hlmnet-front-1")
            .isolation(networked()),
        )
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(
                &back_root,
                Process::new("/bin/sh").args(["-c", "printf 'BACK\\n' | nc -l -p 9201"]),
            )
            .name("hlmnet-back-1")
            .isolation(networked()),
        )
        .await?;
    networks
        .connect("hlmnet_front", "hlmnet-app-1", EndpointSpec::default().name("app"))
        .await?;
    let second = networks
        .connect("hlmnet_back", "hlmnet-app-1", EndpointSpec::default().name("app"))
        .await;
    second?;
    Multinet { containers }.finish().await
}

struct Multinet<'a> {
    containers: &'a Containers,
}

impl Multinet<'_> {
    async fn finish(&self) -> Result<(), Error> {
        let networks = self.containers.networks();
        check(
            networks
                .list()
                .await?
                .iter()
                .any(|network| network.name == "hlmnet_front"),
            "front-network-created",
        )?;
        check(
            networks
                .list()
                .await?
                .iter()
                .any(|network| network.name == "hlmnet_back"),
            "back-network-created",
        )?;
        check(
            networks
                .inspect("hlmnet_front")
                .await?
                .endpoints
                .values()
                .any(|endpoint| endpoint.name == "app"),
            "app-on-front",
        )?;
        networks
            .connect(
                "hlmnet_front",
                "hlmnet-front-1",
                EndpointSpec::default().name("front-peer"),
            )
            .await?;
        networks
            .connect(
                "hlmnet_back",
                "hlmnet-back-1",
                EndpointSpec::default().name("back-peer"),
            )
            .await?;
        check(
            networks
                .inspect("hlmnet_back")
                .await?
                .endpoints
                .values()
                .any(|endpoint| endpoint.name == "app"),
            "app-on-back",
        )?;
        self.containers.start("hlmnet-front-1").await?;
        self.containers.start("hlmnet-back-1").await?;
        self.containers.start("hlmnet-app-1").await?;
        let executions = self.containers.executions();
        let execution = executions
            .create(
                "hlmnet-app-1",
                ExecSpec::new(Process::new("/bin/sh").args([
                    "-c",
                    "printf '%s-%s\\n' \"$(nc -w 3 front-peer 9200)\" \"$(nc -w 3 back-peer 9201)\"",
                ])),
            )
            .await?;
        let mut session = executions.start(&execution.id).await?;
        let output = tokio::time::timeout(Duration::from_secs(10), async {
            let mut output = Vec::new();
            while let Some(entry) = session.next().await? {
                if entry.stream == Stream::Stdout {
                    output.extend(entry.bytes);
                }
            }
            Ok::<_, hl_container::Error>(output)
        })
        .await??;
        check(output == b"FRONT-BACK\n", "routes-both-networks")?;
        for name in ["hlmnet-app-1", "hlmnet-front-1", "hlmnet-back-1"] {
            self.containers.remove_force(name).await?;
        }
        networks.remove("hlmnet_front").await?;
        networks.remove("hlmnet_back").await?;
        check(self.containers.list().await?.is_empty(), "teardown-empty")?;
        Ok(())
    }
}

fn networked() -> Isolation {
    Isolation {
        sandbox: Sandbox::Disabled,
        network_isolated: false,
        ..Isolation::default()
    }
}

fn check(value: bool, name: &'static str) -> Result<(), Error> {
    if value {
        println!("PASS compose/{name}");
        Ok(())
    } else {
        Err(name.into())
    }
}
