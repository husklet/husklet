// Contract tests drive a whole container lifecycle in one future; its size is the test, not a defect.
#![allow(clippy::large_futures)]

//! Public lifecycle acceptance contracts against a pinned Alpine root filesystem.

use hl_container::{
    Check, Config, ContainerSpec, Containers, ExitStatus, Guest, HealthStatus, Healthcheck, Isolation, Process,
    Sandbox, Signal,
};
use std::{future::Future, path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn hangup_reaches_the_guest_signal_handler() -> Result<(), Error> {
    let fixture = bounded("HUP fixture", Fixture::new()).await?;
    let name = "lifecycle-hup";
    let outcome = bounded("HUP lifecycle", async {
        let process = Process::new("/bin/sh").args([
            "-c",
            "trap 'echo GOT_HUP; exit 0' HUP; echo READY; while true; do sleep 1; done",
        ]);
        fixture.containers.create(fixture.spec(name, process)).await?;
        fixture.containers.start(name).await?;
        fixture.wait_for_output(name, b"READY").await?;
        fixture.containers.signal(name, Signal::Hangup).await?;
        let status = fixture.containers.wait(name).await?;
        let logs = fixture.containers.logs(name).await?;
        require(status == ExitStatus::Code(0), "HUP handler exit status mismatch")?;
        require(contains(&logs.stdout, b"GOT_HUP"), "HUP trap output missing")
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn configured_quit_reaches_the_guest_signal_handler() -> Result<(), Error> {
    let fixture = bounded("QUIT fixture", Fixture::new()).await?;
    let name = "lifecycle-quit";
    let outcome = bounded("QUIT lifecycle", async {
        let process = Process::new("/bin/sh").args([
            "-c",
            "trap 'echo GOT_QUIT; exit 0' QUIT; trap 'echo GOT_TERM; exit 3' TERM; echo READY; while true; do sleep 1; done",
        ]);
        fixture
            .containers
            .create(fixture.spec(name, process).stop_signal(Signal::Quit))
            .await?;
        fixture.containers.start(name).await?;
        fixture.wait_for_output(name, b"READY").await?;
        let status = fixture.containers.stop(name, Duration::from_secs(5)).await?;
        let logs = fixture.containers.logs(name).await?;
        require(status == ExitStatus::Code(0), "QUIT handler exit status mismatch")?;
        require(contains(&logs.stdout, b"GOT_QUIT"), "QUIT trap output missing")?;
        require(
            !contains(&logs.stdout, b"GOT_TERM"),
            "default SIGTERM was delivered instead of SIGQUIT",
        )
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn pause_stops_guest_progress_until_unpause() -> Result<(), Error> {
    let fixture = bounded("pause fixture", Fixture::new()).await?;
    let name = "lifecycle-pause";
    let outcome = bounded("pause lifecycle", async {
        let process = Process::new("/bin/sh").args(["-c", "while true; do printf x >> /tmp/progress; sleep .05; done"]);
        fixture.containers.create(fixture.spec(name, process)).await?;
        fixture.containers.start(name).await?;
        let progress = fixture.rootfs.join("tmp/progress");
        wait_for_size(&progress, 2).await?;
        fixture.containers.pause(name).await?;
        let paused = std::fs::metadata(&progress)?.len();
        tokio::time::sleep(Duration::from_millis(250)).await;
        require(
            std::fs::metadata(&progress)?.len() == paused,
            "guest progressed while paused",
        )?;
        fixture.containers.unpause(name).await?;
        wait_for_size(&progress, paused + 1).await
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn health_probes_reach_healthy_and_unhealthy_states() -> Result<(), Error> {
    let fixture = bounded("health fixture", Fixture::new()).await?;
    for (name, program, expected) in [
        ("lifecycle-healthy", "/bin/true", HealthStatus::Healthy),
        ("lifecycle-unhealthy", "/bin/false", HealthStatus::Unhealthy),
    ] {
        let outcome = bounded("health lifecycle", async {
            let check = Healthcheck::new(Check::Command(Process::new(program)))
                .interval(Duration::from_millis(100))
                .timeout(Duration::from_secs(3))
                .retries(2);
            fixture
                .containers
                .create(
                    fixture
                        .spec(name, Process::new("/bin/sleep").args(["300"]))
                        .healthcheck(check),
                )
                .await?;
            fixture.containers.start(name).await?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if fixture
                        .containers
                        .inspect(name)
                        .await?
                        .health
                        .as_ref()
                        .is_some_and(|health| health.status == expected)
                    {
                        return Ok::<_, hl_container::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await??;
            Ok(())
        })
        .await;
        finish(outcome, cleanup(&fixture.containers, name).await)?;
    }
    Ok(())
}

struct Fixture {
    _work: tempfile::TempDir,
    rootfs: std::path::PathBuf,
    containers: Containers,
    guest: Guest,
}

impl Fixture {
    async fn new() -> Result<Self, Error> {
        let work = tempfile::tempdir()?;
        let rootfs = work.path().join("rootfs");
        unpack(&rootfs)?;
        let containers = Containers::builder(Config::new(work.path().join("state")))
            .build()
            .await?;
        Ok(Self {
            _work: work,
            rootfs,
            containers,
            guest: guest()?,
        })
    }

    fn spec(&self, name: &str, process: Process) -> ContainerSpec {
        ContainerSpec::from_directory(&self.rootfs, process)
            .name(name)
            .guest(self.guest)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                read_only_root: false,
                network_isolated: true,
                seccomp_baseline: hl_container::SeccompBaseline::Container,
            })
    }

    async fn wait_for_output(&self, name: &str, marker: &[u8]) -> Result<(), Error> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if contains(&self.containers.logs(name).await?.stdout, marker) {
                    return Ok::<_, hl_container::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }
}

async fn wait_for_size(path: &Path, minimum: u64) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() >= minimum) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| format!("{} did not reach {minimum} bytes", path.display()))?;
    Ok(())
}

fn guest() -> Result<Guest, Error> {
    match std::env::var("HL_SCENARIO_TARGET") {
        Ok(value) if value == "amd64" => Ok(Guest::X86_64),
        Ok(value) if value == "arm64" => Ok(Guest::Aarch64),
        Err(std::env::VarError::NotPresent) => Ok(Guest::Aarch64),
        Ok(value) => Err(format!("unsupported HL_SCENARIO_TARGET {value:?}").into()),
        Err(error) => Err(error.into()),
    }
}

fn unpack(destination: &Path) -> Result<(), Error> {
    let source = std::env::var_os("HL_ALPINE_ARCHIVE").ok_or("HL_ALPINE_ARCHIVE must name the pinned rootfs")?;
    std::fs::create_dir(destination)?;
    let archive = std::fs::File::open(source)?;
    tar::Archive::new(flate2::read::GzDecoder::new(archive)).unpack(destination)?;
    Ok(())
}

fn contains(bytes: &[u8], value: &[u8]) -> bool {
    bytes.windows(value.len()).any(|window| window == value)
}

fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition { Ok(()) } else { Err(message.into()) }
}

async fn bounded<T>(label: &str, future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    tokio::time::timeout(OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| format!("{label} exceeded {OPERATION_TIMEOUT:?}"))?
}

async fn cleanup(containers: &Containers, name: &str) -> Result<(), Error> {
    tokio::time::timeout(CLEANUP_TIMEOUT, containers.remove_force(name))
        .await
        .map_err(|_| format!("cleanup for {name} exceeded {CLEANUP_TIMEOUT:?}"))??;
    Ok(())
}

fn finish(outcome: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (outcome, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
