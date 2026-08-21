//! User-defined network, endpoint allocation, naming, and isolation workflow.

use hl_container::{ContainerSpec, Containers, EndpointSpec, Isolation, NetworkSpec, Process, Subnet};
use std::net::Ipv4Addr;
use tempfile::TempDir;

use super::fixture;

type Error = Box<dyn std::error::Error>;

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    let roots = TempDir::new()?;
    let server_root = fixture::rootfs(roots.path(), "server")?;
    let client_root = fixture::rootfs(roots.path(), "client")?;
    let other_root = fixture::rootfs(roots.path(), "other")?;

    let networks = containers.networks();
    let primary = networks
        .create(NetworkSpec::bridge(
            "hlnet",
            Subnet::new(Ipv4Addr::new(172, 31, 0, 0), 24)?,
        ))
        .await?;
    require(
        networks.list().await?.iter().any(|network| network.name == "hlnet"),
        "network-created",
    )?;

    containers
        .create(
            ContainerSpec::from_directory(
                &server_root,
                Process::new("/bin/sh").args([
                    "-c",
                    "printf 'ready\\n'; printf 'bridge-ok\\n' | nc -l -p 8080; printf 'bridge-ok\\n' | nc -l -p 8081",
                ]),
            )
            .guest(fixture::guest())
            .name("net-srv")
            .isolation(networked()),
        )
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(&client_root, Process::new("/bin/sh").args(["-c", "exit 0"]))
                .guest(fixture::guest())
                .name("net-cli")
                .isolation(networked()),
        )
        .await?;
    let server = networks
        .connect("hlnet", "net-srv", EndpointSpec::default().name("net-srv"))
        .await?;
    let address = server.address.ok_or("server endpoint omitted its address")?;
    containers.remove_force("net-cli").await?;
    containers
        .create(
            ContainerSpec::from_directory(
                &client_root,
                Process::new("/bin/sh").args(["-c", &format!("nc net-srv 8080; nc {address} 8081")]),
            )
            .guest(fixture::guest())
            .name("net-cli")
            .isolation(networked()),
        )
        .await?;
    let client = networks
        .connect("hlnet", "net-cli", EndpointSpec::default().name("net-cli"))
        .await?;
    Topology { containers }
        .members(primary.id.as_str(), &server, &client)
        .await?;

    let secondary = networks
        .create(NetworkSpec::bridge(
            "hlnet2",
            Subnet::new(Ipv4Addr::new(172, 31, 1, 0), 24)?,
        ))
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(&other_root, Process::new("/bin/sh"))
                .guest(fixture::guest())
                .name("net-other")
                .isolation(networked()),
        )
        .await?;
    networks
        .connect("hlnet2", "net-other", EndpointSpec::default().name("net-other"))
        .await?;
    require(
        !networks
            .inspect(secondary.id.as_str())
            .await?
            .endpoints
            .values()
            .any(|endpoint| endpoint.name == "net-srv"),
        "cross-network-isolated",
    )?;

    let topology = Topology { containers };
    topology.verify().await?;
    topology.remove().await
}

struct Topology<'a> {
    containers: &'a Containers,
}

impl Topology<'_> {
    async fn members(
        &self,
        network: &str,
        server: &hl_container::Endpoint,
        client: &hl_container::Endpoint,
    ) -> Result<(), Error> {
        require(server.address.is_some(), "server-has-ip")?;
        require(
            client.address.is_some() && client.address != server.address,
            "client-has-distinct-ip",
        )?;
        let inspected = self.containers.networks().inspect(network).await?;
        require(
            inspected.endpoints.values().any(|endpoint| endpoint.name == "net-srv"),
            "network-inspect-lists-member",
        )
    }

    async fn verify(&self) -> Result<(), Error> {
        self.containers.start("net-srv").await?;
        wait_for_output(self.containers, "net-srv", b"ready\n").await?;
        require(
            self.containers.inspect("net-srv").await?.state.is_active(),
            "server-running",
        )?;
        self.containers.start("net-cli").await?;
        let status = self.containers.wait("net-cli").await?;
        let output = self.containers.logs("net-cli").await?;
        if status != hl_container::ExitStatus::Code(0) {
            let server = self.containers.inspect("net-srv").await?;
            let server_output = self.containers.logs("net-srv").await?;
            return Err(format!(
                "client-exit status={status:?} stdout={:?} stderr={:?} server={:?} server_stdout={:?} server_stderr={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                server.state,
                String::from_utf8_lossy(&server_output.stdout),
                String::from_utf8_lossy(&server_output.stderr),
            )
            .into());
        }
        require(true, "client-exit")?;
        require(output.stdout == b"bridge-ok\nbridge-ok\n", "reach-by-name-and-ip")?;
        require(
            self.containers.wait("net-srv").await? == hl_container::ExitStatus::Code(0),
            "server-exit",
        )
    }

    async fn remove(&self) -> Result<(), Error> {
        for name in ["net-srv", "net-cli", "net-other"] {
            self.containers.remove_force(name).await?;
        }
        let networks = self.containers.networks();
        networks.remove("hlnet").await?;
        networks.remove("hlnet2").await?;
        require(self.containers.list().await?.is_empty(), "cleanup-empty")
    }
}

fn networked() -> Isolation {
    Isolation {
        network_isolated: false,
        ..Isolation::default()
    }
}

async fn wait_for_output(containers: &Containers, name: &str, suffix: &[u8]) -> Result<(), Error> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if containers.logs(name).await?.stdout.ends_with(suffix) {
                return Ok::<_, hl_container::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

fn require(value: bool, name: &'static str) -> Result<(), Error> {
    if value {
        println!("PASS docker-net/{name}");
        Ok(())
    } else {
        Err(name.into())
    }
}
