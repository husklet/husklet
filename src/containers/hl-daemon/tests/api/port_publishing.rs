//! Published host port lifecycle and collision handling.

use crate::api::support::{published, require};
use hl_container::{ContainerSpec, Containers, Isolation, Process, Sandbox};
use std::{path::Path, time::Duration};
use tokio::time::sleep;

pub(crate) async fn run(
    containers: &Containers,
    rootfs: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let reservation = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let host = reservation.local_addr()?.port();
    drop(reservation);
    let publish = hl_container::Publication::tcp(std::net::Ipv4Addr::LOCALHOST, host, 24567)?;
    let server = |name: &str| {
        ContainerSpec::from_directory(
            rootfs,
            Process::new("/bin/sh").args([
                "-c",
                "while true; do echo published-ok | nc -l -p 24567 -w 1; done",
            ]),
        )
        .name(name)
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            network_isolated: false,
            ..Isolation::default()
        })
        .publish(publish)
    };
    let first = containers.create(server("published-first")).await?;
    let second = containers.create(server("published-second")).await?;
    let result = async {
        containers.start(first.id.as_str()).await?;
        sleep(Duration::from_millis(300)).await;
        require(
            published(std::net::Ipv4Addr::LOCALHOST, host).await? == b"published-ok\n",
            "published TCP port returned the wrong payload",
        )?;
        require(
            published("127.0.0.2".parse()?, host).await.is_err(),
            "loopback publication accepted connections on a different host address",
        )?;
        require(
            containers.start(second.id.as_str()).await.is_err()
                && matches!(
                    containers.inspect(second.id.as_str()).await?.state,
                    hl_container::ContainerState::Created
                ),
            "host-port collision published a running container",
        )?;
        containers.stop(first.id.as_str(), Duration::ZERO).await?;
        containers.start(second.id.as_str()).await?;
        sleep(Duration::from_millis(300)).await;
        require(
            published(std::net::Ipv4Addr::LOCALHOST, host).await? == b"published-ok\n",
            "stopping a container did not release its published host port",
        )
    }
    .await;
    let _ = containers.stop(first.id.as_str(), Duration::ZERO).await;
    let _ = containers.stop(second.id.as_str(), Duration::ZERO).await;
    result
}
