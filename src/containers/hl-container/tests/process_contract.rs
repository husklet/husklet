// Contract tests drive a whole container lifecycle in one future; its size is the test, not a defect.
#![allow(clippy::large_futures)]

//! Public guest-process contracts against a pinned Alpine root filesystem.

use hl_container::{
    Config, ContainerSpec, Containers, ExecSpec, ExecState, ExitStatus, Guest, Isolation, Process, Sandbox,
};
use std::{future::Future, path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

struct LaunchCase {
    name: &'static str,
    process: Process,
    status: ExitStatus,
    stdout: &'static [u8],
    stderr: &'static [u8],
    hostname: Option<&'static str>,
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn launch_contracts() -> Result<(), Error> {
    within_deadline(async {
        let fixture = Fixture::new().await?;
        for case in launch_cases() {
            fixture.run(case).await?;
        }
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn sigterm_stop() -> Result<(), Error> {
    within_deadline(async {
        let fixture = Fixture::new().await?;
        let name = "process-signal";
        let process = Process::new("/bin/sh").args([
            "-c",
            "trap 'printf GOT_TERM; exit 0' TERM; printf READY; while :; do :; done",
        ]);
        fixture.containers.create(fixture.spec(name, process)).await?;
        fixture.containers.start(name).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.containers.logs(name).await?.stdout == b"READY" {
                    return Ok::<_, hl_container::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;

        let status = fixture.containers.stop(name, Duration::from_secs(5)).await?;
        let logs = fixture.containers.logs(name).await?;
        fixture.containers.remove(name).await?;
        if status != ExitStatus::Code(0) || logs.stdout != b"READYGOT_TERM" {
            return Err(format!(
                "clean stop mismatch: status={status:?} stdout={:?}",
                String::from_utf8_lossy(&logs.stdout)
            )
            .into());
        }
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn exec_contracts() -> Result<(), Error> {
    within_deadline(async {
        let fixture = Fixture::new().await?;
        let name = "process-exec";
        fixture
            .containers
            .create(fixture.spec(name, Process::new("/bin/sleep").args(["30"])))
            .await?;
        fixture.containers.start(name).await?;

        let outcome = async {
            check_exec(
                &fixture.containers,
                name,
                Process::new("/bin/echo").args(["EXEC_OK"]),
                b"EXEC_OK\n",
            )
            .await?;
            check_exec(
                &fixture.containers,
                name,
                Process::new("/bin/sh")
                    .args(["-c", "printf %s \"$EE\""])
                    .env("EE", "zz"),
                b"zz",
            )
            .await?;
            check_exec(
                &fixture.containers,
                name,
                Process::new("/bin/sh").args(["-c", "printf shared > /tmp/x"]),
                b"",
            )
            .await?;
            check_exec(
                &fixture.containers,
                name,
                Process::new("/bin/cat").args(["/tmp/x"]),
                b"shared",
            )
            .await
        }
        .await;
        let cleanup = fixture.containers.remove_force(name).await.map(|_| ());
        outcome?;
        cleanup?;
        Ok(())
    })
    .await
}

async fn within_deadline<T>(future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    tokio::time::timeout(Duration::from_secs(45), future)
        .await
        .map_err(|_| "process contract exceeded 45 seconds")?
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

    async fn run(&self, case: LaunchCase) -> Result<(), Error> {
        let mut spec = self.spec(case.name, case.process);
        if let Some(hostname) = case.hostname {
            spec = spec.hostname(hostname);
        }
        self.containers.create(spec).await?;
        self.containers.start(case.name).await?;
        let status = self.containers.wait(case.name).await?;
        let logs = self.containers.logs(case.name).await?;
        self.containers.remove(case.name).await?;
        if status != case.status || logs.stdout != case.stdout || logs.stderr != case.stderr {
            return Err(format!(
                "{} mismatch: status={status:?} stdout={:?} stderr={:?}",
                case.name,
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            )
            .into());
        }
        Ok(())
    }
}

async fn check_exec(containers: &Containers, container: &str, process: Process, expected: &[u8]) -> Result<(), Error> {
    let executions = containers.executions();
    let exec = executions.create(container, ExecSpec::new(process)).await?;
    let mut session = executions.start(&exec.id).await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(record) = session.next().await? {
        match record.stream {
            hl_container::Stream::Stdout => stdout.extend(record.bytes),
            hl_container::Stream::Stderr => stderr.extend(record.bytes),
        }
    }
    let exec = executions.inspect(&exec.id).await?;
    if !matches!(
        exec.state,
        ExecState::Exited {
            result: ExitStatus::Code(0),
            ..
        }
    ) || stdout != expected
        || !stderr.is_empty()
    {
        return Err(format!(
            "exec behavior mismatch: state={:?} stdout={stdout:?} stderr={stderr:?}",
            exec.state
        )
        .into());
    }
    Ok(())
}

fn launch_cases() -> Vec<LaunchCase> {
    vec![
        LaunchCase::new(
            "process-env-passthrough",
            Process::new("/bin/printenv").args(["HL_ENV"]).env("HL_ENV", "hello123"),
        )
        .stdout(b"hello123\n"),
        LaunchCase::shell("process-env", "printf '%s' \"$A-$B\"")
            .env("A", "1")
            .env("B", "2")
            .stdout(b"1-2"),
        LaunchCase::new("process-workdir", Process::new("/bin/pwd"))
            .working_dir("/etc")
            .stdout(b"/etc\n"),
        LaunchCase::new("process-workdir-created", Process::new("/bin/pwd"))
            .working_dir("/made/here")
            .stdout(b"/made/here\n"),
        LaunchCase::new("process-exit-zero", Process::new("/bin/true")),
        LaunchCase::shell("process-exit-nonzero", "exit 7").status(ExitStatus::Code(7)),
        LaunchCase::shell("process-exit-rc", "exit 5").status(ExitStatus::Code(5)),
        LaunchCase::shell("process-streams", "printf 'OUTLINE\n'; printf 'ERRLINE\n' >&2")
            .stdout(b"OUTLINE\n")
            .stderr(b"ERRLINE\n"),
        LaunchCase::shell("process-pid", "printf 'PID=%s\n' \"$$\"").stdout(b"PID=1\n"),
        LaunchCase::new("process-uid", Process::new("/usr/bin/id").args(["-u"])).stdout(b"0\n"),
        LaunchCase::new("process-hostname", Process::new("/bin/hostname"))
            .hostname("hlbox")
            .stdout(b"hlbox\n"),
    ]
}

impl LaunchCase {
    fn new(name: &'static str, process: Process) -> Self {
        Self {
            name,
            process,
            status: ExitStatus::Code(0),
            stdout: b"",
            stderr: b"",
            hostname: None,
        }
    }

    fn shell(name: &'static str, command: &'static str) -> Self {
        Self::new(name, Process::new("/bin/sh").args(["-c", command]))
    }

    fn env(mut self, name: &str, value: &str) -> Self {
        self.process = self.process.env(name, value);
        self
    }

    fn working_dir(mut self, value: &str) -> Self {
        self.process = self.process.working_dir(value);
        self
    }

    const fn status(mut self, value: ExitStatus) -> Self {
        self.status = value;
        self
    }

    const fn stdout(mut self, value: &'static [u8]) -> Self {
        self.stdout = value;
        self
    }

    const fn stderr(mut self, value: &'static [u8]) -> Self {
        self.stderr = value;
        self
    }

    const fn hostname(mut self, value: &'static str) -> Self {
        self.hostname = Some(value);
        self
    }
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
