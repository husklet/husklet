//! Container lifecycle compatibility cases.

use crate::report::ScenarioBatch;
use hl_container::{
    Check, ContainerSpec, ContainerState, Containers, ExitStatus, HealthStatus, Healthcheck,
    Isolation, Process, RestartPolicy, Sandbox, Signal,
};
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;
const IDS: [&str; 17] = [
    "lifecycle/create-start",
    "lifecycle/stop",
    "lifecycle/kill-signal",
    "lifecycle/restart",
    "lifecycle/pause-unpause",
    "lifecycle/wait",
    "lifecycle/rm",
    "lifecycle/rm-multi",
    "lifecycle/rm-multi-force",
    "lifecycle/rm-force",
    "lifecycle/rename",
    "lifecycle/stop-signal-quit",
    "lifecycle/stop-signal-inspect",
    "lifecycle/restart-on-failure-count",
    "lifecycle/unless-stopped-manual",
    "lifecycle/healthcheck-healthy",
    "lifecycle/healthcheck-unhealthy",
];

pub(crate) fn group() -> crate::contract::Group {
    crate::contract::Group::new(
        "lifecycle",
        IDS.into_iter()
            .map(|id| crate::contract::Scenario::new(id, "alpine:3.20").api(id))
            .collect(),
    )
}

pub(crate) async fn run(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    let scenarios = group()
        .scenarios
        .into_iter()
        .map(|value| (value.id, value))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut reports = ScenarioBatch::new("lifecycle")?;
    let mut failures = Vec::new();
    for id in IDS {
        let scenario = &scenarios[id];
        let Some(attempt) = reports.begin(scenario)? else {
            println!("RESUME {id}");
            continue;
        };
        let result = (Case { containers, rootfs }).run(id).await;
        reports.complete(scenario, attempt, &result)?;
        match result {
            Ok(()) => println!("PASS {id}"),
            Err(error) => {
                println!("FAIL {id}: {error}");
                failures.push(format!("{id}: {error}"));
            }
        }
    }
    reports.finish(Vec::new())?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

struct Case<'a> {
    containers: &'a Containers,
    rootfs: &'a Path,
}
impl Case<'_> {
    async fn run(&self, id: &str) -> Result<(), Error> {
        match id {
            "lifecycle/create-start" => self.create_start().await,
            "lifecycle/stop" => self.stop().await,
            "lifecycle/kill-signal" => self.kill_signal().await,
            "lifecycle/restart" => self.restart().await,
            "lifecycle/pause-unpause" => self.pause().await,
            "lifecycle/wait" => self.wait().await,
            "lifecycle/rm" => self.remove().await,
            "lifecycle/rm-multi" => self.multi(false).await,
            "lifecycle/rm-multi-force" => self.multi(true).await,
            "lifecycle/rm-force" => self.force().await,
            "lifecycle/rename" => self.rename().await,
            "lifecycle/stop-signal-quit" => self.stop_signal().await,
            "lifecycle/stop-signal-inspect" => self.stop_signal_inspect().await,
            "lifecycle/restart-on-failure-count" => self.retry().await,
            "lifecycle/unless-stopped-manual" => self.unless().await,
            "lifecycle/healthcheck-healthy" => self.health(true).await,
            "lifecycle/healthcheck-unhealthy" => self.health(false).await,
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
    async fn create_start(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec("lc-create", Process::new("/bin/echo").args(["CREATED_RUN"])))
            .await?;
        self.containers.start("lc-create").await?;
        require(
            self.containers.wait("lc-create").await? == ExitStatus::Code(0),
            "exit mismatch",
        )?;
        require(
            self.containers.logs("lc-create").await?.stdout == b"CREATED_RUN\n",
            "output mismatch",
        )
    }
    async fn stop(&self) -> Result<(), Error> {
        self.sleep("lc-stop").await?;
        self.containers
            .stop("lc-stop", Duration::from_secs(2))
            .await?;
        require(
            !self.containers.inspect("lc-stop").await?.state.is_active(),
            "still running",
        )
    }
    async fn kill_signal(&self) -> Result<(), Error> {
        let p = Process::new("/bin/sh").args([
            "-c",
            "trap 'echo GOT_HUP; exit 0' HUP; echo READY; while true; do sleep 1; done",
        ]);
        self.containers.create(self.spec("lc-hup", p)).await?;
        self.containers.start("lc-hup").await?;
        self.marker("lc-hup", b"READY").await?;
        self.containers.signal("lc-hup", Signal::Hangup).await?;
        self.containers.wait("lc-hup").await?;
        require(
            contains(&self.containers.logs("lc-hup").await?.stdout, b"GOT_HUP"),
            "HUP trap missing",
        )
    }
    async fn restart(&self) -> Result<(), Error> {
        self.sleep("lc-restart").await?;
        let first = self.containers.inspect("lc-restart").await?.state;
        self.containers.stop("lc-restart", Duration::ZERO).await?;
        self.containers.start("lc-restart").await?;
        let second = self.containers.inspect("lc-restart").await?.state;
        require(
            first != second && second.is_active(),
            "restart state mismatch",
        )
    }
    async fn pause(&self) -> Result<(), Error> {
        self.sleep("lc-pause").await?;
        self.containers.pause("lc-pause").await?;
        require(
            matches!(
                self.containers.inspect("lc-pause").await?.state,
                ContainerState::Paused { .. }
            ),
            "not paused",
        )?;
        self.containers.unpause("lc-pause").await?;
        require(
            matches!(
                self.containers.inspect("lc-pause").await?.state,
                ContainerState::Running { .. }
            ),
            "not running",
        )
    }
    async fn wait(&self) -> Result<(), Error> {
        self.containers
            .create(self.spec(
                "lc-wait",
                Process::new("/bin/sh").args(["-c", "sleep 1; exit 17"]),
            ))
            .await?;
        self.containers.start("lc-wait").await?;
        require(
            self.containers.wait("lc-wait").await? == ExitStatus::Code(17),
            "wait != 17",
        )
    }
    async fn remove(&self) -> Result<(), Error> {
        self.finished("lc-rm").await?;
        self.containers.remove("lc-rm").await?;
        require(
            self.containers.inspect("lc-rm").await.is_err(),
            "remove left record",
        )
    }
    async fn multi(&self, force: bool) -> Result<(), Error> {
        for suffix in ["a", "b", "c"] {
            let n = format!("lc-multi-{suffix}");
            if force {
                self.sleep(&n).await?;
            } else {
                self.finished(&n).await?;
            }
        }
        for suffix in ["a", "b", "c"] {
            let n = format!("lc-multi-{suffix}");
            if force {
                self.containers.remove_force(&n).await?;
            } else {
                self.containers.remove(&n).await?;
            }
        }
        require(
            !self.containers.list().await?.iter().any(|c| {
                c.spec
                    .name
                    .as_deref()
                    .is_some_and(|n| n.starts_with("lc-multi-"))
            }),
            "multi remove left records",
        )
    }
    async fn force(&self) -> Result<(), Error> {
        self.sleep("lc-force").await?;
        require(
            self.containers.remove("lc-force").await.is_err(),
            "plain remove accepted running",
        )?;
        self.containers.remove_force("lc-force").await?;
        require(
            self.containers.inspect("lc-force").await.is_err(),
            "force remove left record",
        )
    }
    async fn rename(&self) -> Result<(), Error> {
        self.sleep("lc-old").await?;
        self.containers.rename("lc-old", "lc-new").await?;
        require(
            self.containers
                .inspect("lc-new")
                .await?
                .spec
                .name
                .as_deref()
                == Some("lc-new"),
            "new name missing",
        )?;
        require(
            self.containers.inspect("lc-old").await.is_err(),
            "old name resolves",
        )
    }
    async fn stop_signal(&self) -> Result<(), Error> {
        let p = Process::new("/bin/sh").args(["-c", "trap 'echo GOT_QUIT; exit 0' QUIT; trap 'echo GOT_TERM; exit 3' TERM; echo READY; while true; do sleep 1; done"]);
        self.containers
            .create(self.spec("lc-quit", p).stop_signal(Signal::Quit))
            .await?;
        self.containers.start("lc-quit").await?;
        self.marker("lc-quit", b"READY").await?;
        let status = self
            .containers
            .stop("lc-quit", Duration::from_secs(5))
            .await?;
        let logs = self.containers.logs("lc-quit").await?;
        require(
            status == ExitStatus::Code(0) && contains(&logs.stdout, b"GOT_QUIT"),
            "configured SIGQUIT was not delivered",
        )
    }
    async fn stop_signal_inspect(&self) -> Result<(), Error> {
        let spec = self
            .spec("lc-quit-inspect", Process::new("/bin/true"))
            .stop_signal(Signal::Quit);
        self.containers.create(spec).await?;
        require(
            self.containers
                .inspect("lc-quit-inspect")
                .await?
                .spec
                .stop_signal
                == Signal::Quit,
            "configured stop signal was not durable",
        )
    }
    async fn retry(&self) -> Result<(), Error> {
        let spec = self
            .spec("lc-retry", Process::new("/bin/sh").args(["-c", "exit 1"]))
            .restart(RestartPolicy::OnFailure { maximum: Some(2) });
        self.containers.create(spec).await?;
        self.containers.start("lc-retry").await?;
        self.inactive("lc-retry").await?;
        let c = self.containers.inspect("lc-retry").await?;
        require(
            c.restart.count == 2 && !c.state.is_active(),
            "restart count/running mismatch",
        )
    }
    async fn unless(&self) -> Result<(), Error> {
        let spec = self
            .spec("lc-unless", Process::new("/bin/sleep").args(["300"]))
            .restart(RestartPolicy::UnlessStopped);
        self.containers.create(spec).await?;
        self.containers.start("lc-unless").await?;
        self.containers
            .stop("lc-unless", Duration::from_secs(2))
            .await?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        require(
            !self
                .containers
                .inspect("lc-unless")
                .await?
                .state
                .is_active(),
            "manual stop restarted",
        )
    }
    async fn health(&self, good: bool) -> Result<(), Error> {
        let name = if good { "lc-healthy" } else { "lc-unhealthy" };
        let program = if good { "/bin/true" } else { "/bin/false" };
        let check = Healthcheck::new(Check::Command(Process::new(program)))
            .interval(Duration::from_millis(100))
            .timeout(Duration::from_secs(3))
            .retries(2);
        self.containers
            .create(
                self.spec(name, Process::new("/bin/sleep").args(["300"]))
                    .healthcheck(check),
            )
            .await?;
        self.containers.start(name).await?;
        let wanted = if good {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self
                    .containers
                    .inspect(name)
                    .await?
                    .health
                    .as_ref()
                    .is_some_and(|h| h.status == wanted)
                {
                    return Ok::<_, hl_container::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await??;
        Ok(())
    }
    async fn sleep(&self, name: &str) -> Result<(), Error> {
        self.containers
            .create(self.spec(name, Process::new("/bin/sleep").args(["300"])))
            .await?;
        self.containers.start(name).await?;
        Ok(())
    }
    async fn finished(&self, name: &str) -> Result<(), Error> {
        self.containers
            .create(self.spec(name, Process::new("/bin/true")))
            .await?;
        self.containers.start(name).await?;
        self.containers.wait(name).await?;
        Ok(())
    }
    async fn marker(&self, name: &str, value: &[u8]) -> Result<(), Error> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if contains(&self.containers.logs(name).await?.stdout, value) {
                    return Ok::<_, hl_container::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }
    async fn inactive(&self, name: &str) -> Result<(), Error> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !self.containers.inspect(name).await?.state.is_active() {
                    return Ok::<_, hl_container::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        Ok(())
    }
}
fn contains(bytes: &[u8], value: &[u8]) -> bool {
    bytes.windows(value.len()).any(|window| window == value)
}
fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
