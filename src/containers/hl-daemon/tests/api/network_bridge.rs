//! Bridge network connectivity between containers.

use crate::api::support::TIMEOUT;
use hl_container::{ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox};
use std::{path::Path, time::Duration};
use tokio::time::{sleep, timeout};

pub(crate) async fn run(containers: &Containers, rootfs: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let network = containers
        .networks()
        .create(hl_container::NetworkSpec::bridge(
            "runtime-bridge",
            hl_container::Subnet::new("10.77.0.0".parse()?, 24)?,
        ))
        .await?;
    let isolation = Isolation {
        network_isolated: false,
        sandbox: Sandbox::Disabled,
        ..Isolation::default()
    };
    let server = containers
        .create(
            ContainerSpec::from_directory(
                rootfs,
                Process::new("/bin/sh").args(["-c", "while true; do echo bridge-ok | nc -l -p 23456 -w 1; done"]),
            )
            .name("network-server")
            .isolation(isolation),
        )
        .await?;
    let server_endpoint = containers
        .networks()
        .connect(&network.name, server.id.as_str(), hl_container::EndpointSpec::default())
        .await?;
    let server_address = server_endpoint.address.ok_or("bridge server endpoint has no address")?;
    let client_command = format!("nc -w 3 network-server 23456; nc -w 3 {server_address} 23456");
    let client = containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/sh").args(["-c", &client_command]))
                .name("network-client")
                .isolation(isolation),
        )
        .await?;
    containers
        .networks()
        .connect(&network.name, client.id.as_str(), hl_container::EndpointSpec::default())
        .await?;
    containers.start(server.id.as_str()).await?;
    sleep(Duration::from_secs(1)).await;
    containers.start(client.id.as_str()).await?;
    let client_status = timeout(TIMEOUT, containers.wait(client.id.as_str())).await??;
    let client_logs = containers.logs(client.id.as_str()).await?;
    if client_status != ExitStatus::Code(0) {
        return Err(format!(
            "bridge client failed: {client_status:?}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&client_logs.stdout),
            String::from_utf8_lossy(&client_logs.stderr)
        )
        .into());
    }
    let client_logs = containers.logs(client.id.as_str()).await?;
    if client_logs.stdout != b"bridge-ok\nbridge-ok\n" {
        return Err(format!(
            "bridge payload mismatch: stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&client_logs.stdout),
            String::from_utf8_lossy(&client_logs.stderr)
        )
        .into());
    }
    let _ = containers.stop(server.id.as_str(), Duration::ZERO).await?;
    Ok(())
}
