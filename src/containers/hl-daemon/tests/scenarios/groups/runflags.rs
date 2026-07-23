//! Container run-option compatibility cases.

mod docker;

use crate::report::LegacyBatch;
use hl_container::{
    Console, ContainerSpec, Containers, ExitStatus, Isolation, Mount, Process, Resources,
    RestartPolicy, Sandbox, Size,
};
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;
const IDS: [&str; 20] = [
    "runflags/detached-d",
    "runflags/env-e",
    "runflags/publish-p",
    "runflags/publish-p-explicit",
    "runflags/bind-mount-v",
    "runflags/workdir-w",
    "runflags/rm",
    "runflags/name",
    "runflags/entrypoint",
    "runflags/user-uidgid",
    "runflags/user-name",
    "runflags/network-none",
    "runflags/network-bridge",
    "runflags/restart-on-failure",
    "runflags/exit-code",
    "runflags/stdin-i",
    "runflags/tty-t",
    "runflags/memory-accepted",
    "runflags/memory-cgroup-honored",
    "runflags/cpus-accepted",
];

pub(crate) fn group() -> crate::contract::Group {
    crate::contract::Group::new(
        "runflags",
        IDS.into_iter()
            .map(|id| crate::contract::Scenario::new(id, "alpine:3.20").api(id))
            .collect(),
    )
}

pub(crate) async fn run(containers: &Containers, rootfs: &Path, work: &Path) -> Result<(), Error> {
    let scenarios = group()
        .scenarios
        .into_iter()
        .map(|value| (value.id, value))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut reports = LegacyBatch::new("runflags")?;
    let mut failures = Vec::new();
    let selected = std::env::var("HL_SCENARIO_CASE").ok();
    for id in IDS
        .into_iter()
        .filter(|id| selected.as_deref().is_none_or(|selected| selected == *id))
    {
        let scenario = &scenarios[id];
        let Some(attempt) = reports.begin(scenario)? else {
            println!("RESUME {id}");
            continue;
        };
        let result = if matches!(
            id,
            "runflags/publish-p"
                | "runflags/rm"
                | "runflags/user-name"
                | "runflags/network-bridge"
                | "runflags/env-e"
        ) {
            docker::run(id, containers, rootfs, work).await
        } else {
            (Case {
                containers,
                rootfs,
                work,
            })
            .run(id)
            .await
        };
        reports.complete(scenario, attempt, &result)?;
        match result {
            Ok(()) => println!("PASS {id}"),
            Err(error) => {
                println!("FAIL {id}: {error}");
                failures.push(format!("{id}: {error}"));
            }
        }
    }
    reports.finish(selected.into_iter().collect())?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

struct Case<'a> {
    containers: &'a Containers,
    rootfs: &'a Path,
    work: &'a Path,
}
impl Case<'_> {
    async fn run(&self, id: &str) -> Result<(), Error> {
        match id {
            "runflags/detached-d" => self.detached().await,
            "runflags/env-e" => self.command("rf-env", Process::new("/bin/printenv").args(["FOO"]).env("FOO", "barbaz"), b"barbaz\n").await,
            "runflags/publish-p" => Err("automatic host-port assignment is not modeled by the headless API".into()),
            "runflags/publish-p-explicit" => self.publish().await,
            "runflags/bind-mount-v" => self.bind().await,
            "runflags/workdir-w" => self.command("rf-work", Process::new("/bin/pwd").working_dir("/var/spool"), b"/var/spool\n").await,
            "runflags/rm" => Err("automatic removal after exit is not modeled".into()),
            "runflags/name" => self.name().await,
            "runflags/entrypoint" => self.command("rf-entry", Process::new("/bin/echo").args(["ENTRYOVERRIDE"]), b"ENTRYOVERRIDE\n").await,
            "runflags/user-uidgid" => self.command("rf-user", Process::new("/usr/bin/id").user(1000, 1000), b"uid=1000 gid=1000 groups=1000\n").await,
            "runflags/user-name" => Err("named-user resolution is not exposed by the headless specification".into()),
            "runflags/network-none" => self.net_none().await,
            "runflags/network-bridge" => self.net_bridge().await,
            "runflags/restart-on-failure" => self.restart().await,
            "runflags/exit-code" => self.exit().await,
            "runflags/stdin-i" => self.stdin().await,
            "runflags/tty-t" => self.tty().await,
            "runflags/memory-accepted" => self.resource("rf-memory", Resources { memory_bytes: 64 * 1024 * 1024, ..Resources::default() }, "echo MEMFLAG_OK", b"MEMFLAG_OK\n").await,
            "runflags/memory-cgroup-honored" => self.resource("rf-memory-file", Resources { memory_bytes: 64 * 1024 * 1024, ..Resources::default() }, "cat /sys/fs/cgroup/memory.max 2>/dev/null || cat /sys/fs/cgroup/memory/memory.limit_in_bytes", b"67108864\n").await,
            "runflags/cpus-accepted" => self.resource("rf-cpu", Resources { cpu_count: 1, ..Resources::default() }, "echo CPUFLAG_OK", b"CPUFLAG_OK\n").await,
            _ => unreachable!(),
        }
    }
    fn spec(&self, name: &str, process: Process) -> ContainerSpec {
        ContainerSpec::from_directory(self.rootfs, process)
            .name(name)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                network_isolated: true,
                ..Isolation::default()
            })
    }
    async fn command(&self, name: &str, process: Process, stdout: &[u8]) -> Result<(), Error> {
        self.containers.create(self.spec(name, process)).await?;
        self.containers.start(name).await?;
        let status = self.containers.wait(name).await?;
        let logs = self.containers.logs(name).await?;
        require(
            status == ExitStatus::Code(0) && logs.stdout == stdout,
            format!(
                "status={status:?} stdout={:?}",
                String::from_utf8_lossy(&logs.stdout)
            ),
        )
    }
    async fn detached(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec("rf-detached", Process::new("/bin/sleep").args(["30"])))
            .await?;
        self.containers.start("rf-detached").await?;
        require(
            self.containers
                .inspect("rf-detached")
                .await?
                .state
                .is_active(),
            "not running",
        )
    }
    async fn publish(&self) -> Result<(), Error> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let host = listener.local_addr()?.port();
        drop(listener);
        let publish = hl_container::Publication::tcp(std::net::Ipv4Addr::LOCALHOST, host, 9000)?;
        let spec = self
            .spec(
                "rf-publish",
                Process::new("/bin/sh").args([
                    "-c",
                    "while true; do echo EXPLICITOK | nc -l -p 9000 -w 2; done",
                ]),
            )
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                network_isolated: false,
                ..Isolation::default()
            })
            .publish(publish);
        self.containers.create(spec).await?;
        self.containers.start("rf-publish").await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut stream =
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, host)).await?;
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut bytes).await?;
        require(bytes == b"EXPLICITOK\n", "published payload mismatch")
    }
    async fn bind(&self) -> Result<(), Error> {
        let mount = self.work.join("rf-bind");
        std::fs::create_dir_all(&mount)?;
        std::fs::write(mount.join("f"), "SEED\n")?;
        let spec = self
            .spec(
                "rf-bind",
                Process::new("/bin/sh").args(["-c", "cat /m/f; echo WROTE > /m/g"]),
            )
            .mount(Mount::read_write(&mount, "/m"));
        self.containers.create(spec).await?;
        self.containers.start("rf-bind").await?;
        self.containers.wait("rf-bind").await?;
        require(
            self.containers.logs("rf-bind").await?.stdout == b"SEED\n"
                && std::fs::read(mount.join("g"))? == b"WROTE\n",
            "bind visibility mismatch",
        )
    }
    async fn name(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec("rf-named", Process::new("/bin/sleep").args(["30"])))
            .await?;
        require(
            self.containers
                .inspect("rf-named")
                .await?
                .spec
                .name
                .as_deref()
                == Some("rf-named"),
            "name missing",
        )
    }
    async fn net_none(&self) -> Result<(), Error> {
        self.command(
            "rf-none",
            Process::new("/bin/sh").args([
                "-c",
                "ip -o link 2>/dev/null | grep -q eth0 && echo HAS_ETH || echo NO_ETH",
            ]),
            b"NO_ETH\n",
        )
        .await
    }
    async fn net_bridge(&self) -> Result<(), Error> {
        self.command(
            "rf-bridge",
            Process::new("/bin/sh").args([
                "-c",
                "/sbin/ip -o link show eth0 >/dev/null 2>&1 && echo HAS_ETH0",
            ]),
            b"HAS_ETH0\n",
        )
        .await
    }
    async fn restart(&self) -> Result<(), Error> {
        let spec = self
            .spec("rf-restart", Process::new("/bin/sh").args(["-c", "exit 1"]))
            .restart(RestartPolicy::OnFailure { maximum: Some(3) });
        self.containers.create(spec).await?;
        self.containers.start("rf-restart").await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let c = self.containers.inspect("rf-restart").await?;
                if c.restart.count == 3 && !c.state.is_active() {
                    return Ok::<_, hl_container::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await??;
        Ok(())
    }
    async fn exit(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec("rf-exit", Process::new("/bin/sh").args(["-c", "exit 42"])))
            .await?;
        self.containers.start("rf-exit").await?;
        require(
            self.containers.wait("rf-exit").await? == ExitStatus::Code(42),
            "exit != 42",
        )
    }
    async fn stdin(&self) -> Result<(), Error> {
        let process = Process::new("/bin/cat").console(Console {
            stdin: true,
            terminal: None,
        });
        self.containers
            .create(self.spec("rf-stdin", process))
            .await?;
        let session = self.containers.attach("rf-stdin").await?;
        self.containers.start("rf-stdin").await?;
        session.write(b"HELLOSTDIN\n".to_vec()).await?;
        session.close().await;
        self.containers.wait("rf-stdin").await?;
        require(
            self.containers.logs("rf-stdin").await?.stdout == b"HELLOSTDIN\n",
            "stdin mismatch",
        )
    }
    async fn tty(&self) -> Result<(), Error> {
        let process = Process::new("/bin/sh")
            .args(["-c", "test -t 1 && echo IS_TTY || echo NO_TTY"])
            .console(Console {
                stdin: false,
                terminal: Some(Size::new(24, 80)?),
            });
        self.command("rf-tty", process, b"IS_TTY\r\n").await
    }
    async fn resource(
        &self,
        name: &str,
        resources: Resources,
        command: &str,
        output: &[u8],
    ) -> Result<(), Error> {
        let spec = self
            .spec(name, Process::new("/bin/sh").args(["-c", command]))
            .resources(resources);
        self.containers.create(spec).await?;
        self.containers.start(name).await?;
        let status = self.containers.wait(name).await?;
        let logs = self.containers.logs(name).await?;
        require(
            status == ExitStatus::Code(0) && logs.stdout == output,
            format!(
                "status={status:?} stdout={:?}",
                String::from_utf8_lossy(&logs.stdout)
            ),
        )
    }
}
fn require(condition: bool, message: impl Into<String>) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
