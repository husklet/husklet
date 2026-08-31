// Contract tests drive a whole container lifecycle in one future; its size is the test, not a defect.
#![allow(clippy::large_futures)]

//! Public lifecycle acceptance contracts against a pinned Alpine root filesystem.

use hl_container::{
    Check, Config, Console, ContainerSpec, ContainerState, Containers, ExecSpec, ExitStatus, Guest, HealthStatus,
    Healthcheck, Isolation, Process, Sandbox, Signal, Size, Stream, Streams,
};
use std::{future::Future, path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
static LIFECYCLE_PROCESS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn hangup_reaches_the_guest_signal_handler() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
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
        fixture.containers.signal(name, Signal::HANGUP).await?;
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
async fn a_descriptor_duplicated_with_fcntl_is_visible_in_proc_self_fd() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
    let fixture = bounded("fcntl duplicate fixture", Fixture::new()).await?;
    let name = "lifecycle-fcntl-duplicate";
    let outcome = bounded("fcntl duplicate lifecycle", async {
        // An interactive shell moves its job-control terminal to a descriptor of its own with
        // fcntl(F_DUPFD, 10). The glob is expanded by that same shell, so it reports its OWN descriptor
        // table -- a child would not, because the duplicate is close-on-exec.
        let process = Process::new("/bin/sh")
            .args(["-i", "-c", "echo FDS $(echo /proc/self/fd/*)"])
            .console(Console::default().terminal(Size::new(24, 80)?));
        fixture.containers.create(fixture.spec(name, process)).await?;
        fixture.containers.start(name).await?;
        fixture.wait_for_output(name, b"FDS ").await?;
        let logs = fixture.containers.logs(name).await?;
        require(
            contains(&logs.stdout, b"/proc/self/fd/10"),
            "the shell's fcntl(F_DUPFD) duplicate is missing from its own /proc/<pid>/fd",
        )
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn configured_quit_reaches_the_guest_signal_handler() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
    let fixture = bounded("QUIT fixture", Fixture::new()).await?;
    let name = "lifecycle-quit";
    let outcome = bounded("QUIT lifecycle", async {
        let process = Process::new("/bin/sh").args([
            "-c",
            "trap 'echo GOT_QUIT; exit 0' QUIT; trap 'echo GOT_TERM; exit 3' TERM; echo READY; while true; do sleep 1; done",
        ]);
        fixture
            .containers
            .create(fixture.spec(name, process).stop_signal(Signal::QUIT))
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
    let _process = LIFECYCLE_PROCESS.lock().await;
    let fixture = bounded("pause fixture", Fixture::new()).await?;
    let name = "lifecycle-pause";
    let outcome = bounded("pause lifecycle", async {
        let process = Process::new("/bin/sh").args(["-c", "while true; do printf x >> /tmp/progress; sleep .05; done"]);
        fixture.containers.create(fixture.spec(name, process)).await?;
        fixture.containers.start(name).await?;
        let progress = fixture.rootfs.join("tmp/progress");
        wait_for_size(&progress, 2).await?;
        for _ in 0..3 {
            fixture.containers.pause(name).await?;
            let paused = std::fs::metadata(&progress)?.len();
            tokio::time::sleep(Duration::from_millis(250)).await;
            require(
                std::fs::metadata(&progress)?.len() == paused,
                "guest progressed while paused",
            )?;
            fixture.containers.unpause(name).await?;
            wait_for_size(&progress, paused + 1).await?;
        }
        Ok(())
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn checkpoint_restore_preserves_filesystem_and_container_control() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
    let fixture = bounded("checkpoint fixture", Fixture::new()).await?;
    let name = "lifecycle-checkpoint";
    let outcome = bounded("checkpoint lifecycle", async {
        let process = Process::new("/bin/sh").args([
            "-c",
            // Keep this CPU-bound so capture still interrupts translated guest work, but publish progress
            // independently of interpreter throughput. The old 10,000-iteration cadence deterministically
            // missed the five-second amd64 observation budget before checkpointing began.
            "echo durable > /tmp/checkpoint-marker; printf S >> /tmp/checkpoint-starts; while true; do printf x >> /tmp/checkpoint-progress; i=0; while [ $i -lt 100 ]; do i=$((i + 1)); done; done",
        ]);
        fixture.containers.create(fixture.spec(name, process)).await?;
        fixture.containers.start(name).await?;
        let progress = fixture.rootfs.join("tmp/checkpoint-progress");
        wait_for_size(&progress, 2).await?;
        require(
            matches!(fixture.containers.inspect(name).await?.state, ContainerState::Running { .. }),
            "checkpoint fixture was not running before capture",
        )?;
        fixture.containers.checkpoint(name, Duration::from_secs(10)).await?;
        fixture.containers.start(name).await?;
        let resumed = std::fs::metadata(&progress)?.len();
        wait_for_size(&progress, resumed + 1).await?;
        require(
            std::fs::read_to_string(fixture.rootfs.join("tmp/checkpoint-marker"))? == "durable\n",
            "checkpoint restore lost the guest filesystem marker",
        )?;
        require(
            std::fs::read(fixture.rootfs.join("tmp/checkpoint-starts"))? == b"S",
            "checkpoint restore fresh-started the guest process",
        )?;
        fixture.containers.pause(name).await?;
        let paused = std::fs::metadata(&progress)?.len();
        tokio::time::sleep(Duration::from_millis(250)).await;
        require(
            std::fs::metadata(&progress)?.len() == paused,
            "restored guest progressed while paused",
        )?;
        fixture.containers.unpause(name).await?;
        wait_for_size(&progress, paused + 1).await
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn checkpoint_restore_reattaches_the_same_interactive_execution() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
    let fixture = bounded("exec checkpoint fixture", Fixture::new()).await?;
    let name = "lifecycle-exec-checkpoint";
    let outcome = bounded("exec checkpoint lifecycle", async {
        fixture
            .containers
            .create(fixture.spec(name, Process::new("/bin/sleep").args(["10000"])))
            .await?;
        fixture.containers.start(name).await?;
        let execution = fixture
            .containers
            .executions()
            .create(
                name,
                ExecSpec::new(
                    Process::new("/bin/sh")
                        .args(["-c", "while IFS= read -r line; do printf 'reply:%s\\n' \"$line\"; done"])
                        .console(Console::default().terminal(Size::new(24, 80)?)),
                )
                .streams(Streams {
                    stdin: true,
                    stdout: true,
                    stderr: true,
                }),
            )
            .await?;
        let mut session = fixture.containers.executions().start(&execution.id).await?;
        session.write("before\n").await?;
        require(
            read_session_until(&mut session, b"reply:before").await?,
            "pre-capture exec did not answer",
        )?;

        fixture.containers.checkpoint_all(Duration::from_secs(10)).await?;
        drop(session);
        fixture.containers.start(name).await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let failures = fixture.containers.executions().restore_checkpoints().await?;
        if !failures.is_empty() {
            return Err(format!("restored execution was refused: {failures:?}").into());
        }
        let mut session = fixture.containers.executions().attach(&execution.id, None).await?;
        session.write("after\n").await?;
        require(
            read_session_until(&mut session, b"reply:after").await?,
            "reattached exec did not answer",
        )
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE and HL_EXECUTABLE_MAPPING_FIXTURE"]
async fn auto_backend_falls_back_after_an_executable_mapping_without_losing_output() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
    let fixture = bounded("executable mapping fixture", Fixture::new()).await?;
    let source = std::env::var_os("HL_EXECUTABLE_MAPPING_FIXTURE")
        .ok_or("HL_EXECUTABLE_MAPPING_FIXTURE must name the settled static fixture")?;
    let destination = fixture.rootfs.join("work/executable-mapping");
    std::fs::create_dir_all(destination.parent().ok_or("fixture destination has no parent")?)?;
    std::fs::copy(source, &destination)?;
    let mut permissions = std::fs::metadata(&destination)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&destination, permissions)?;

    let name = "lifecycle-executable-mapping";
    let outcome = bounded("executable mapping lifecycle", async {
        fixture
            .containers
            .create(fixture.spec(name, Process::new("/work/executable-mapping")))
            .await?;
        fixture.containers.start(name).await?;
        let status = fixture.containers.wait(name).await?;
        let logs = fixture.containers.logs(name).await?;
        require(status == ExitStatus::Code(0), "executable mapping fixture failed")?;
        require(
            logs.stdout == b"work h=6acbb551769b4d75\nwork h=6acbb551769b4d75\nwork h=6acbb551769b4d75\n",
            "fallback execution changed the fixture output",
        )
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn checkpoint_restore_restarts_interrupted_sleep_syscalls() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
    let fixture = bounded("sleep checkpoint fixture", Fixture::new()).await?;
    let name = "lifecycle-checkpoint-sleep";
    let outcome = bounded("sleep checkpoint lifecycle", async {
        let process = Process::new("/bin/sh").args([
            "-c",
            "sleep 1000 & a=$!; sleep 1000 & b=$!; sleep 1000 & c=$!; \
             while kill -0 \"$a\" && kill -0 \"$b\" && kill -0 \"$c\"; do \
                 printf x >> /tmp/checkpoint-sleep-progress; sleep .05; \
             done; printf 'sleep child lost\n' > /tmp/checkpoint-sleep-failure; exit 91",
        ]);
        fixture.containers.create(fixture.spec(name, process)).await?;
        fixture.containers.start(name).await?;
        let progress = fixture.rootfs.join("tmp/checkpoint-sleep-progress");
        wait_for_size(&progress, 2).await?;
        fixture.containers.checkpoint(name, Duration::from_secs(10)).await?;
        fixture.containers.start(name).await?;
        let resumed = std::fs::metadata(&progress)?.len();
        wait_for_size(&progress, resumed + 1).await?;
        require(
            !fixture.rootfs.join("tmp/checkpoint-sleep-failure").exists(),
            "checkpoint surfaced its interrupt to a sleeping child",
        )
    })
    .await;
    finish(outcome, cleanup(&fixture.containers, name).await)
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn health_probes_reach_healthy_and_unhealthy_states() -> Result<(), Error> {
    let _process = LIFECYCLE_PROCESS.lock().await;
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

async fn read_session_until(session: &mut hl_container::Session, marker: &[u8]) -> Result<bool, Error> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut output = Vec::new();
        while let Some(entry) = session.next().await? {
            if entry.stream == Stream::Stdout {
                output.extend(entry.bytes);
            }
            if contains(&output, marker) {
                return Ok::<_, hl_container::Error>(true);
            }
        }
        Ok(false)
    })
    .await?
    .map_err(Into::into)
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
