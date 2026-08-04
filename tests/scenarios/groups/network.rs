//! Exact Docker-network plus single-container network contracts.

use super::{registry, runner::Runner, volume};
use crate::report::ScenarioBatch;
use hl_client::{
    model::{
        Attachment, CreateContainer, EndpointConfig, EndpointsConfig, ExecConfig, ExecStart,
        HostConfig, NetworkConnect, NetworkCreate, NetworkingConfig,
    },
    Client, Config,
};
use hl_container::Containers;
use hl_daemon::Daemon;
use std::{path::Path, time::Duration};
use tempfile::TempDir;
use tokio::{io::AsyncWriteExt as _, sync::oneshot};

type Error = Box<dyn std::error::Error>;

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    let selected = std::env::var("HL_SCENARIO_CASE").ok();
    let mut failures = Vec::new();
    if selected
        .as_deref()
        .is_none_or(|id| id.starts_with("dockernet/"))
    {
        if let Err(error) = docker(containers, selected.as_deref()).await {
            failures.push(error.to_string());
        }
    }
    if selected
        .as_deref()
        .is_none_or(|id| id.starts_with("networking/"))
    {
        if let Err(error) = Runner::from_env(containers)?
            .run(registry::networking::group())
            .await
        {
            failures.push(error.to_string());
        }
    }
    if selected
        .as_deref()
        .is_none_or(|id| id.starts_with("netinstall/"))
    {
        if let Err(error) = Runner::from_env(containers)?
            .run(registry::netinstall::group())
            .await
        {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

async fn docker(containers: &Containers, selected: Option<&str>) -> Result<(), Error> {
    let scenarios = registry::dockernet::group()
        .scenarios
        .into_iter()
        .map(|value| (value.id, value))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut reports = ScenarioBatch::new("dockernet")?;
    let work = TempDir::new()?;
    let archive = volume::archive(work.path())?;
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
    let client = Client::with_config(Config::unix(&socket).timeout(Duration::from_secs(45)))?;
    client
        .images()
        .load(tokio::fs::File::open(archive).await?)
        .await?;
    let cases = [
        "dockernet/create-ls",
        "dockernet/rm",
        "dockernet/connect",
        "dockernet/inspect",
        "dockernet/reach-by-name",
        "dockernet/reach-by-name-late",
        "dockernet/create-multi-alias",
        "dockernet/host-mode",
    ];
    let mut failures = Vec::new();
    let selected_cases: Vec<_> = cases
        .into_iter()
        .filter(|id| selected.is_none_or(|value| value == *id))
        .collect();
    let total = selected_cases.len();
    for id in selected_cases {
        let scenario = &scenarios[id];
        let Some(attempt) = reports.begin(scenario)? else {
            println!("RESUME {id}");
            continue;
        };
        let result = docker_case(&client, id).await;
        reports.complete(scenario, attempt, &result)?;
        match result {
            Ok(()) => println!("PASS {id}"),
            Err(error) => {
                println!("FAIL {id}: {error}");
                failures.push(format!("{id}: {error}"));
            }
        }
    }
    let _ = shutdown.send(());
    reports.finish(selected.into_iter().map(str::to_owned).collect())?;
    server.await??;
    println!(
        "dockernet scenarios: {} pass; {} fail; {} total",
        total.saturating_sub(failures.len()),
        failures.len(),
        total
    );
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

async fn docker_case(client: &Client, id: &str) -> Result<(), Error> {
    let suffix = id.rsplit('/').next().unwrap_or("case");
    let network = format!("contract-{suffix}");
    let created = client
        .networks()
        .create(&NetworkCreate {
            name: network.clone(),
            driver: "bridge".into(),
            ..NetworkCreate::default()
        })
        .await?;
    let result = match id {
        "dockernet/create-ls" => check(
            client
                .networks()
                .list()
                .await?
                .iter()
                .any(|item| item.name == network),
            "network omitted from list",
        ),
        "dockernet/rm" => {
            client.networks().remove(&network, false).await?;
            check(
                !client
                    .networks()
                    .list()
                    .await?
                    .iter()
                    .any(|item| item.name == network),
                "removed network remained listed",
            )
        }
        "dockernet/inspect" => check(
            client.networks().inspect(&created.id).await?.name == network,
            "network inspect mismatch",
        ),
        "dockernet/connect" => connect_case(client, &network).await,
        "dockernet/reach-by-name" => {
            tokio::time::timeout(Duration::from_secs(20), reach(client, &network, false)).await?
        }
        "dockernet/reach-by-name-late" => {
            tokio::time::timeout(Duration::from_secs(20), reach(client, &network, true)).await?
        }
        "dockernet/create-multi-alias" => multi_alias(client, &network).await,
        "dockernet/host-mode" => host_mode(client).await,
        _ => Err("unknown dockernet case".into()),
    };
    cleanup(client, &network).await;
    if id == "dockernet/create-multi-alias" {
        let _ = client
            .networks()
            .remove(&format!("{network}-second"), false)
            .await;
    }
    result
}

async fn host_mode(client: &Client) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        stream.write_all(b"HOSTNETOK\n").await
    });
    let mut request = request(&format!("nc -w 5 127.0.0.1 {port}"));
    request.host_config = Some(HostConfig {
        network_mode: "host".into(),
        ..HostConfig::default()
    });
    let created = client
        .containers()
        .create(&request, Some("host-mode-client"))
        .await?;
    check(
        created.warnings.is_empty(),
        "host mode unexpectedly reported a warning without publications",
    )?;
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    tokio::time::timeout(Duration::from_secs(10), server).await???;
    let inspect = client.containers().inspect(&created.id).await?;
    check(
        status.status_code == 0,
        "host-mode client exited unsuccessfully",
    )?;
    check(
        logs.stdout.windows(9).any(|value| value == b"HOSTNETOK"),
        "guest could not reach a host-loopback listener",
    )?;
    check(
        inspect.host_config.network_mode == "host" && inspect.network_settings.networks.is_empty(),
        "host-mode inspect fields were inconsistent",
    )
}

async fn multi_alias(client: &Client, first: &str) -> Result<(), Error> {
    let second = format!("{first}-second");
    client
        .networks()
        .create(&NetworkCreate {
            name: second.clone(),
            driver: "bridge".into(),
            ..NetworkCreate::default()
        })
        .await?;
    let multi = |command: &str, aliases: [&str; 2]| {
        let mut request = request(command);
        request.host_config = Some(HostConfig {
            network_mode: first.into(),
            ..HostConfig::default()
        });
        request.networking_config = Some(NetworkingConfig {
            endpoints_config: EndpointsConfig(
                [
                    (
                        first.into(),
                        EndpointConfig {
                            aliases: vec![aliases[0].into()],
                            ..EndpointConfig::default()
                        },
                    ),
                    (
                        second.clone(),
                        EndpointConfig {
                            aliases: vec![aliases[1].into()],
                            ..EndpointConfig::default()
                        },
                    ),
                ]
                .into_iter()
                .collect(),
            ),
        });
        request
    };
    let server = client
        .containers()
        .create(
            &multi("printf 'MULTIOK\\n' | nc -l -p 9000", ["front", "database"]),
            Some("multi-server"),
        )
        .await?;
    let consumer = client
        .containers()
        .create(
            &multi("sleep 120", ["consumer-front", "consumer-back"]),
            Some("multi-client"),
        )
        .await?;
    client.containers().start(&server.id).await?;
    client.containers().start(&consumer.id).await?;
    let execution = client
        .executions()
        .create(
            &consumer.id,
            &ExecConfig {
                attach: Attachment {
                    stdout: true,
                    stderr: true,
                    ..Attachment::default()
                },
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "grep database /etc/hosts; nc -v -w 3 database 9000".into(),
                ],
                ..ExecConfig::default()
            },
        )
        .await?;
    let mut session = client
        .executions()
        .start(&execution.id, &ExecStart::default())
        .await?;
    let mut output = Vec::new();
    while let Some(frame) = session.next().await? {
        output.extend_from_slice(frame.bytes());
    }
    let server_state = client.containers().inspect(&server.id).await?;
    let server_logs = client.containers().logs(&server.id, true, true).await?;
    let result = check(
        output.windows(7).any(|value| value == b"MULTIOK"),
        &format!(
            "multi-network alias was not reachable: client={} server_state={:?} server_stdout={} server_stderr={}",
            String::from_utf8_lossy(&output),
            server_state.state,
            String::from_utf8_lossy(&server_logs.stdout),
            String::from_utf8_lossy(&server_logs.stderr),
        ),
    );
    result
}

async fn connect_case(client: &Client, network: &str) -> Result<(), Error> {
    let container = client
        .containers()
        .create(&request("sleep 120"), Some("network-connect"))
        .await?;
    client
        .networks()
        .connect(
            network,
            &NetworkConnect {
                container: container.id.clone(),
                ..NetworkConnect::default()
            },
        )
        .await?;
    let inspect = client.networks().inspect(network).await?;
    check(
        inspect.containers.contains_key(&container.id),
        "network inspect omitted container endpoint",
    )
}

async fn reach(client: &Client, network: &str, late: bool) -> Result<(), Error> {
    let client_container = client
        .containers()
        .create(
            &network_request("sleep 120", network),
            Some(if late { "late-client" } else { "name-client" }),
        )
        .await?;
    if late {
        client.containers().start(&client_container.id).await?;
    }
    let server = client
        .containers()
        .create(
            &network_request("printf 'NAMEOK\\n' | nc -l -p 9000", network),
            Some(if late { "late-server" } else { "name-server" }),
        )
        .await?;
    client.containers().start(&server.id).await?;
    if !late {
        client.containers().start(&client_container.id).await?;
    }
    let server_name = if late { "late-server" } else { "name-server" };
    let execution = client
        .executions()
        .create(
            &client_container.id,
            &ExecConfig {
                attach: Attachment {
                    stdout: true,
                    stderr: true,
                    ..Attachment::default()
                },
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("nc -w 3 {server_name} 9000"),
                ],
                ..ExecConfig::default()
            },
        )
        .await?;
    let mut session = client
        .executions()
        .start(&execution.id, &ExecStart::default())
        .await?;
    let mut output = Vec::new();
    while let Some(frame) = session.next().await? {
        output.extend_from_slice(frame.bytes());
    }
    check(
        output.windows(6).any(|value| value == b"NAMEOK"),
        "peer was not reachable by name",
    )
}

fn request(command: &str) -> CreateContainer {
    CreateContainer {
        image: volume::IMAGE.into(),
        cmd: Some(vec!["/bin/sh".into(), "-c".into(), command.into()]),
        ..CreateContainer::default()
    }
}

fn network_request(command: &str, network: &str) -> CreateContainer {
    let mut request = request(command);
    request.host_config = Some(HostConfig {
        network_mode: network.into(),
        ..HostConfig::default()
    });
    request.networking_config = Some(NetworkingConfig {
        endpoints_config: EndpointsConfig(std::collections::BTreeMap::from([(
            network.into(),
            EndpointConfig::default(),
        )])),
    });
    request
}

async fn cleanup(client: &Client, network: &str) {
    if let Ok(items) = client.containers().list(true).await {
        for item in items {
            let _ = client
                .containers()
                .remove(&item.metadata.id, true, true)
                .await;
        }
    }
    let _ = client.networks().remove(network, true).await;
}

fn check(ok: bool, message: &str) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
async fn wait(path: &Path) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}
