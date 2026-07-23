use crate::api::support::{raw_http, wait_for_path};
use crate::report::LegacyBatch;
use hl_client::Client;
use hl_container::{
    ContainerSpec, ContainerState, Containers, EndpointSpec, Isolation, Mount, NetworkSpec,
    Process, Publication, Sandbox, Stream, Subnet,
};
use hl_daemon::Daemon;
use std::{net::Ipv4Addr, path::Path, time::Duration};
use tempfile::TempDir;
use tokio::sync::oneshot;

type Error = Box<dyn std::error::Error>;

struct Observe<'a> {
    containers: &'a Containers,
    rootfs: &'a Path,
    work: &'a Path,
}

impl<'a> Observe<'a> {
    const fn new(containers: &'a Containers, rootfs: &'a Path, work: &'a Path) -> Self {
        Self {
            containers,
            rootfs,
            work,
        }
    }

    async fn run(&self) -> Result<(), Error> {
        let scenarios = crate::registry::observe::group()
            .scenarios
            .into_iter()
            .map(|value| (value.id, value))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut reports = LegacyBatch::new("observe")?;
        let mut failures = Vec::new();
        for id in OBSERVE_IDS {
            let scenario = &scenarios[id];
            let Some(attempt) = reports.begin(scenario)? else {
                println!("RESUME {id}");
                continue;
            };
            let result = self.case(id).await;
            reports.complete(scenario, attempt, &result)?;
            match result {
                Ok(()) => println!("PASS {id}"),
                Err(error) => {
                    println!("FAIL {id}: {error}");
                    failures.push(format!("{id}: {error}"));
                }
            }
        }
        println!(
            "observe scenarios: {} passed; {} failed; 16 total",
            16 - failures.len(),
            failures.len()
        );
        reports.finish(Vec::new())?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n").into())
        }
    }

    async fn case(&self, id: &str) -> Result<(), Error> {
        match id {
            "observe/inspect-state" => self.inspect_state().await,
            "observe/inspect-config-env" => self.inspect_env().await,
            "observe/inspect-cmd" => self.inspect_command().await,
            "observe/inspect-mounts" => self.inspect_mounts().await,
            "observe/inspect-network-ip" => self.inspect_network().await,
            "observe/ps-running" => self.ps_running().await,
            "observe/ps-all-exited" => self.ps_exited().await,
            "observe/ps-ports" => Self::ps_ports(),
            "observe/logs" => self.logs().await,
            "observe/logs-tail" => self.logs_tail().await,
            "observe/logs-follow" => self.logs_follow().await,
            "observe/top" => self.endpoint("top").await,
            "observe/stats-oneshot" => self.endpoint("stats?stream=false").await,
            "observe/port" => Self::port(),
            "observe/container-prune-filter" => self.prune_filter(false).await,
            "observe/system-prune-filter-reject" => self.prune_filter(true).await,
            _ => unreachable!("validated observe scenario ID"),
        }
    }

    fn spec(&self, name: &str, process: Process) -> ContainerSpec {
        ContainerSpec::from_directory(self.rootfs, process)
            .name(name)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                read_only_root: false,
                network_isolated: true,
            })
    }

    async fn running(&self, name: &str) -> Result<(), Error> {
        self.containers
            .create(self.spec(name, Process::new("/bin/sleep").args(["60"])))
            .await?;
        self.containers.start(name).await?;
        Ok(())
    }

    async fn inspect_state(&self) -> Result<(), Error> {
        self.running("observe-state").await?;
        let state = self.containers.inspect("observe-state").await?.state;
        self.containers
            .stop("observe-state", Duration::ZERO)
            .await?;
        if matches!(state, ContainerState::Running { .. }) {
            Ok(())
        } else {
            Err(format!("state={state:?}; expected running").into())
        }
    }

    async fn inspect_env(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec(
                "observe-env",
                Process::new("/bin/true").env("MARKERENV", "zz9"),
            ))
            .await?;
        let value = self
            .containers
            .inspect("observe-env")
            .await?
            .spec
            .process
            .env
            .get("MARKERENV")
            .cloned();
        if value.as_deref() == Some("zz9") {
            Ok(())
        } else {
            Err(format!("MARKERENV={value:?}").into())
        }
    }

    async fn inspect_command(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec("observe-command", Process::new("/bin/echo").args(["hicmd"])))
            .await?;
        let process = self
            .containers
            .inspect("observe-command")
            .await?
            .spec
            .process;
        if process.program.ends_with("echo") && process.args == ["hicmd"] {
            Ok(())
        } else {
            Err(format!("process={process:?}").into())
        }
    }

    async fn inspect_mounts(&self) -> Result<(), Error> {
        let source = self.work.join("observe-mount");
        std::fs::create_dir_all(&source)?;
        self.containers
            .create(
                self.spec("observe-mounts", Process::new("/bin/true"))
                    .mount(Mount::read_write(source, "/mnt")),
            )
            .await?;
        let mounts = self.containers.inspect("observe-mounts").await?.spec.mounts;
        if mounts.iter().any(|mount| mount.target == Path::new("/mnt")) {
            Ok(())
        } else {
            Err("inspect omitted /mnt destination".into())
        }
    }

    async fn inspect_network(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec("observe-network", Process::new("/bin/true")))
            .await?;
        self.containers
            .networks()
            .create(NetworkSpec::bridge(
                "observe-net",
                Subnet::new("10.88.0.0".parse()?, 24)?,
            ))
            .await?;
        let endpoint = self
            .containers
            .networks()
            .connect("observe-net", "observe-network", EndpointSpec::default())
            .await?;
        if endpoint.address.is_some() {
            Ok(())
        } else {
            Err("network endpoint has no IPv4 address".into())
        }
    }

    async fn ps_running(&self) -> Result<(), Error> {
        self.running("observe-ps-running").await?;
        let running = self.containers.list().await?.into_iter().any(|container| {
            container.spec.name.as_deref() == Some("observe-ps-running")
                && matches!(container.state, ContainerState::Running { .. })
        });
        self.containers
            .stop("observe-ps-running", Duration::ZERO)
            .await?;
        if running {
            Ok(())
        } else {
            Err("running container absent from list".into())
        }
    }

    async fn ps_exited(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec("observe-ps-exited", Process::new("/bin/true")))
            .await?;
        self.containers.start("observe-ps-exited").await?;
        self.containers.wait("observe-ps-exited").await?;
        if matches!(
            self.containers.inspect("observe-ps-exited").await?.state,
            ContainerState::Exited { .. }
        ) {
            Ok(())
        } else {
            Err("exited container absent from all-container list".into())
        }
    }

    fn ps_ports() -> Result<(), Error> {
        match Publication::tcp(Ipv4Addr::LOCALHOST, 0, 80) {
            Ok(_) => Ok(()),
            Err(error) => Err(format!("automatic host-port allocation rejected: {error}").into()),
        }
    }

    async fn logs(&self) -> Result<(), Error> {
        let name = "observe-logs";
        self.containers
            .create(self.spec(
                name,
                Process::new("/bin/sh").args(["-c", "echo LOGLINE1; echo LOGLINE2"]),
            ))
            .await?;
        self.containers.start(name).await?;
        self.containers.wait(name).await?;
        let logs = self.containers.logs(name).await?;
        if logs.stdout == b"LOGLINE1\nLOGLINE2\n" {
            Ok(())
        } else {
            Err(format!("stdout={:?}", logs.stdout).into())
        }
    }

    async fn logs_tail(&self) -> Result<(), Error> {
        let name = "observe-tail";
        self.containers
            .create(self.spec(
                name,
                Process::new("/bin/sh").args(["-c", "for i in 1 2 3 4 5; do echo L$i; done"]),
            ))
            .await?;
        self.containers.start(name).await?;
        self.containers.wait(name).await?;
        let stdout = self.containers.logs(name).await?.stdout;
        let lines = stdout.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        if lines.ends_with(&[b"L4".as_slice(), b"L5".as_slice(), b"".as_slice()]) {
            Ok(())
        } else {
            Err(format!("stdout={stdout:?}").into())
        }
    }

    async fn logs_follow(&self) -> Result<(), Error> {
        let name = "observe-follow";
        self.containers
            .create(self.spec(
                name,
                Process::new("/bin/sh").args(["-c", "echo FOLLOW1; sleep 1; echo FOLLOW2"]),
            ))
            .await?;
        let mut session = self.containers.follow(name).await?;
        self.containers.start(name).await?;
        let mut output = Vec::new();
        while let Some(entry) = session.next().await? {
            if entry.stream == Stream::Stdout {
                output.extend(entry.bytes);
            }
        }
        if output == b"FOLLOW1\nFOLLOW2\n" {
            Ok(())
        } else {
            Err(format!("stdout={output:?}").into())
        }
    }

    async fn endpoint(&self, endpoint: &str) -> Result<(), Error> {
        let name = endpoint.split('?').next().unwrap_or(endpoint);
        let container = format!("observe-{name}");
        self.running(&container).await?;
        let work = TempDir::new()?;
        let socket = work.path().join("daemon.sock");
        let (shutdown, stopped) = oneshot::channel();
        let server = tokio::spawn(
            Daemon::new(self.containers.clone())
                .server(&socket)
                .serve_with_shutdown(async move {
                    let _ = stopped.await;
                }),
        );
        wait_for_path(&socket).await?;
        let request = format!(
            "GET /v1.43/containers/{container}/{endpoint} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
        let response = raw_http(&socket, request.as_bytes()).await?;
        let _ = shutdown.send(());
        server.await??;
        self.containers.stop(&container, Duration::ZERO).await?;
        if !response.starts_with("HTTP/1.1 200") {
            Err(format!("response={}", response.lines().next().unwrap_or("empty")).into())
        } else if name == "top" && !response.contains("sleep") {
            Err("top response omitted the running sleep process".into())
        } else if name == "stats"
            && !(response.contains("\"pids_stats\"")
                && response.contains("\"cpu_stats\"")
                && response.contains("\"memory_stats\""))
        {
            Err("stats response omitted required resource sections".into())
        } else {
            Ok(())
        }
    }

    fn port() -> Result<(), Error> {
        match Publication::tcp(Ipv4Addr::LOCALHOST, 0, 80) {
            Ok(publish) if publish.host_ip == Ipv4Addr::LOCALHOST => Ok(()),
            Ok(publish) => Err(format!("reported host address={}", publish.host_ip).into()),
            Err(error) => Err(format!("automatic published port rejected: {error}").into()),
        }
    }

    async fn prune_filter(&self, system: bool) -> Result<(), Error> {
        self.containers
            .create(
                self.spec("filter-keep", Process::new("/bin/true"))
                    .label("retention", "keep"),
            )
            .await?;
        self.containers
            .create(
                self.spec("filter-drop", Process::new("/bin/true"))
                    .label("retention", "drop"),
            )
            .await?;
        let socket = self.work.join(if system {
            "system-prune.sock"
        } else {
            "container-prune.sock"
        });
        let (shutdown, stopped) = oneshot::channel();
        let server = tokio::spawn(
            Daemon::new(self.containers.clone())
                .server(&socket)
                .serve_with_shutdown(async move {
                    let _ = stopped.await;
                }),
        );
        wait_for_path(&socket).await?;
        let client = Client::unix(&socket)?;
        let filters = if system {
            [("label!".to_owned(), vec!["retention=keep".to_owned()])].into()
        } else {
            [("label".to_owned(), vec!["retention=drop".to_owned()])].into()
        };
        if system {
            client.system().prune_with(false, &filters).await?;
        } else {
            client.containers().prune_with(&filters).await?;
        }
        let kept = self.containers.inspect("filter-keep").await.is_ok();
        let drop_exists = self.containers.inspect("filter-drop").await.is_ok();
        let _ = self.containers.remove("filter-keep").await;
        let _ = self.containers.remove("filter-drop").await;
        let _ = shutdown.send(());
        server.await??;
        if kept && !drop_exists {
            Ok(())
        } else {
            Err("prune filter selected or mutated the wrong containers".into())
        }
    }
}

const OBSERVE_IDS: [&str; 16] = [
    "observe/inspect-state",
    "observe/inspect-config-env",
    "observe/inspect-cmd",
    "observe/inspect-mounts",
    "observe/inspect-network-ip",
    "observe/ps-running",
    "observe/ps-all-exited",
    "observe/ps-ports",
    "observe/logs",
    "observe/logs-tail",
    "observe/logs-follow",
    "observe/top",
    "observe/stats-oneshot",
    "observe/port",
    "observe/container-prune-filter",
    "observe/system-prune-filter-reject",
];

pub(crate) async fn run(containers: &Containers, rootfs: &Path, work: &Path) -> Result<(), Error> {
    Observe::new(containers, rootfs, work).run().await
}
