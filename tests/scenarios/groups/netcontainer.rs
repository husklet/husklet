use crate::report::ScenarioBatch;
use hl_container::{
    ContainerSpec, Containers, EndpointSpec, ExitStatus, Isolation, NetworkSpec, Process, Sandbox, Subnet,
};
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

const CASES: [(&str, &str); 5] = [
    ("netcontainer/redis-by-name", "redis:alpine"),
    ("netcontainer/redis-by-ip", "redis:alpine"),
    ("netcontainer/nc-echo-by-name", "busybox:latest"),
    ("netcontainer/ping-by-name", "alpine:latest"),
    ("netcontainer/isolation-off-network", "alpine:latest"),
];

pub(crate) fn group() -> crate::contract::Group {
    crate::contract::Group::new(
        "netcontainer",
        CASES
            .into_iter()
            .map(|(id, image)| crate::contract::Scenario::new(id, image).api(id))
            .collect(),
    )
}

struct NetworkCases<'a> {
    containers: &'a Containers,
    alpine: &'a Path,
    redis_name: &'a Path,
    redis_ip: &'a Path,
    busybox: &'a Path,
}

impl<'a> NetworkCases<'a> {
    const fn new(
        containers: &'a Containers,
        alpine: &'a Path,
        redis_name: &'a Path,
        redis_ip: &'a Path,
        busybox: &'a Path,
    ) -> Self {
        Self {
            containers,
            alpine,
            redis_name,
            redis_ip,
            busybox,
        }
    }

    async fn run(&self) -> Result<(), Error> {
        let scenarios = group()
            .scenarios
            .into_iter()
            .map(|value| (value.id, value))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut reports = ScenarioBatch::new("netcontainer")?;
        let mut failures = Vec::new();
        for id in CASES.map(|value| value.0) {
            let scenario = &scenarios[id];
            let Some(attempt) = reports.begin(scenario)? else {
                println!("RESUME {id}");
                continue;
            };
            let (network, containers, mut result): (&str, &[&str], Result<(), Error>) = match id {
                "netcontainer/redis-by-name" => (
                    "redis-name-net",
                    &["redis-name-server", "redis-name-set", "redis-name-get"],
                    self.redis_name().await,
                ),
                "netcontainer/redis-by-ip" => (
                    "redis-ip-net",
                    &["redis-ip-server", "redis-ip-set", "redis-ip-client"],
                    self.redis_ip().await,
                ),
                "netcontainer/nc-echo-by-name" => {
                    ("nc-name-net", &["nc-name-server", "nc-name-client"], self.nc().await)
                }
                "netcontainer/ping-by-name" => (
                    "ping-name-net",
                    &["ping-name-server", "ping-name-client"],
                    self.ping().await,
                ),
                "netcontainer/isolation-off-network" => (
                    "isolation-net",
                    &["isolation-server", "isolation-on", "isolation-off"],
                    self.isolation().await,
                ),
                _ => unreachable!(),
            };
            for container in containers {
                self.cleanup(container).await;
            }
            if let Err(error) = self.containers.networks().remove(network).await {
                result = Err(format!("network cleanup failed: {error}").into());
            }
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
            "network-container scenarios: {} passed; {} failed; 5 total",
            5 - failures.len(),
            failures.len()
        );
        reports.finish(Vec::new())?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n").into())
        }
    }

    fn spec(rootfs: &Path, name: &str, process: Process) -> ContainerSpec {
        ContainerSpec::from_directory(rootfs, process)
            .name(name)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                read_only_root: false,
                network_isolated: false,
            })
    }

    async fn network(&self, name: &str, octet: u8) -> Result<(), Error> {
        self.containers
            .networks()
            .create(NetworkSpec::bridge(
                name,
                Subnet::new(format!("10.89.{octet}.0").parse()?, 24)?,
            ))
            .await?;
        Ok(())
    }

    async fn attach(&self, network: &str, container: &str) -> Result<hl_container::Endpoint, Error> {
        Ok(self
            .containers
            .networks()
            .connect(network, container, EndpointSpec::default())
            .await?)
    }

    async fn one_shot(
        &self,
        rootfs: &Path,
        network: Option<&str>,
        name: &str,
        process: Process,
    ) -> Result<(ExitStatus, Vec<u8>), Error> {
        self.containers.create(Self::spec(rootfs, name, process)).await?;
        if let Some(network) = network {
            self.attach(network, name).await?;
        }
        self.containers.start(name).await?;
        let Ok(status) = tokio::time::timeout(Duration::from_secs(10), self.containers.wait(name)).await else {
            if let Ok(logs) = self.containers.logs(name).await {
                eprintln!("{name} stdout: {}", String::from_utf8_lossy(&logs.stdout));
                eprintln!("{name} stderr: {}", String::from_utf8_lossy(&logs.stderr));
            }
            let _ = self.containers.stop(name, Duration::ZERO).await;
            let _ = self.containers.remove_force(name).await;
            return Err(format!("container {name} did not exit within 10 seconds").into());
        };
        let status = status?;
        let logs = self.containers.logs(name).await?;
        self.containers.remove_force(name).await?;
        if status != ExitStatus::Code(0) && !logs.stderr.is_empty() {
            eprintln!("{name} stderr: {}", String::from_utf8_lossy(&logs.stderr));
        }
        Ok((status, logs.stdout))
    }

    async fn cleanup(&self, name: &str) {
        let _ = self.containers.stop(name, Duration::ZERO).await;
        let _ = self.containers.remove_force(name).await;
    }

    async fn redis_server(&self, rootfs: &Path, network: &str, name: &str) -> Result<hl_container::Endpoint, Error> {
        self.containers
            .create(Self::spec(rootfs, name, Process::new("/usr/local/bin/redis-server")))
            .await?;
        let endpoint = self.attach(network, name).await?;
        self.containers.start(name).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        if matches!(
            self.containers.inspect(name).await?.state,
            hl_container::ContainerState::Running { .. }
        ) {
            Ok(endpoint)
        } else {
            let inspection = self.containers.inspect(name).await?;
            let logs = self.containers.logs(name).await?;
            Err(format!(
                "redis server {name} exited before accepting connections: state={:?} stdout={:?} stderr={:?}",
                inspection.state,
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            )
            .into())
        }
    }

    async fn redis_name(&self) -> Result<(), Error> {
        let network = "redis-name-net";
        self.network(network, 1).await?;
        self.redis_server(self.redis_name, network, "redis-name-server").await?;
        let set = self
            .one_shot(
                self.redis_name,
                Some(network),
                "redis-name-set",
                Process::new("/usr/local/bin/redis-cli").args(["-h", "redis-name-server", "set", "foo", "barbar"]),
            )
            .await;
        let get = self
            .one_shot(
                self.redis_name,
                Some(network),
                "redis-name-get",
                Process::new("/usr/local/bin/redis-cli").args(["-h", "redis-name-server", "get", "foo"]),
            )
            .await;
        self.cleanup("redis-name-server").await;
        let (set_status, _) = set?;
        let (get_status, output) = get?;
        if set_status == ExitStatus::Code(0) && get_status == ExitStatus::Code(0) && output == b"barbar\n" {
            Ok(())
        } else {
            Err(format!("set={set_status:?} get={get_status:?} stdout={output:?}").into())
        }
    }

    async fn redis_ip(&self) -> Result<(), Error> {
        let network = "redis-ip-net";
        self.network(network, 2).await?;
        let endpoint = self.redis_server(self.redis_ip, network, "redis-ip-server").await?;
        let address = endpoint.address.ok_or("redis endpoint has no address")?;
        let address = address.to_string();
        let set = self
            .one_shot(
                self.redis_ip,
                Some(network),
                "redis-ip-set",
                Process::new("/usr/local/bin/redis-cli").args(["-h", &address, "set", "k", "ipval"]),
            )
            .await;
        let get = self
            .one_shot(
                self.redis_ip,
                Some(network),
                "redis-ip-client",
                Process::new("/usr/local/bin/redis-cli").args(["-h", &address, "get", "k"]),
            )
            .await;
        self.cleanup("redis-ip-server").await;
        let (set_status, _) = set?;
        let (status, output) = get?;
        if set_status == ExitStatus::Code(0) && status == ExitStatus::Code(0) && output == b"ipval\n" {
            Ok(())
        } else {
            Err(format!("status={status:?} stdout={output:?}").into())
        }
    }

    async fn nc_server(&self, network: &str, name: &str) -> Result<(), Error> {
        self.containers
            .create(Self::spec(
                self.busybox,
                name,
                Process::new("/bin/sh").args(["-c", "while true; do echo NCREPLY | nc -l -p 7000; done"]),
            ))
            .await?;
        self.attach(network, name).await?;
        self.containers.start(name).await?;
        tokio::time::sleep(Duration::from_millis(800)).await;
        Ok(())
    }

    async fn nc(&self) -> Result<(), Error> {
        let network = "nc-name-net";
        self.network(network, 3).await?;
        self.nc_server(network, "nc-name-server").await?;
        let result = self
            .one_shot(
                self.busybox,
                Some(network),
                "nc-name-client",
                Process::new("/bin/nc").args(["-w", "3", "nc-name-server", "7000"]),
            )
            .await;
        self.cleanup("nc-name-server").await;
        let (status, output) = result?;
        if status == ExitStatus::Code(0) && output == b"NCREPLY\n" {
            Ok(())
        } else {
            Err(format!("status={status:?} stdout={output:?}").into())
        }
    }

    async fn sleep_server(&self, network: &str, name: &str) -> Result<(), Error> {
        self.containers
            .create(Self::spec(self.alpine, name, Process::new("/bin/sleep").args(["60"])))
            .await?;
        self.attach(network, name).await?;
        self.containers.start(name).await?;
        Ok(())
    }

    async fn ping(&self) -> Result<(), Error> {
        let network = "ping-name-net";
        self.network(network, 4).await?;
        self.sleep_server(network, "ping-name-server").await?;
        let result = self
            .one_shot(
                self.alpine,
                Some(network),
                "ping-name-client",
                Process::new("/bin/ping").args(["-c", "1", "-W", "3", "ping-name-server"]),
            )
            .await;
        self.cleanup("ping-name-server").await;
        let (status, _) = result?;
        if status == ExitStatus::Code(0) {
            Ok(())
        } else {
            Err(format!("ping status={status:?}").into())
        }
    }

    async fn isolation(&self) -> Result<(), Error> {
        let network = "isolation-net";
        self.network(network, 5).await?;
        self.sleep_server(network, "isolation-server").await?;
        let on = self
            .one_shot(
                self.alpine,
                Some(network),
                "isolation-on",
                Process::new("/bin/ping").args(["-c", "1", "-W", "3", "isolation-server"]),
            )
            .await;
        let off = self
            .one_shot(
                self.alpine,
                None,
                "isolation-off",
                Process::new("/bin/ping").args(["-c", "1", "-W", "3", "isolation-server"]),
            )
            .await;
        self.cleanup("isolation-server").await;
        let (on_status, _) = on?;
        let (off_status, _) = off?;
        if on_status == ExitStatus::Code(0) && off_status != ExitStatus::Code(0) {
            Ok(())
        } else {
            Err(format!("on={on_status:?} off={off_status:?}").into())
        }
    }
}

pub(crate) async fn run(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    let redis_name = crate::fixture::Fixture::materialize("redis:alpine").await?;
    let redis_ip = crate::fixture::Fixture::materialize("redis:alpine").await?;
    let busybox = crate::fixture::Fixture::materialize("busybox:latest").await?;
    let result = NetworkCases::new(containers, rootfs, redis_name.path(), redis_ip.path(), busybox.path())
        .run()
        .await;
    redis_name.release()?;
    redis_ip.release()?;
    busybox.release()?;
    result
}
