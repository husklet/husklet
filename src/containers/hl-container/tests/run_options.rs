// Contract tests drive a whole container lifecycle in one future; its size is the test, not a defect.
#![allow(clippy::large_futures)]

//! Container run-option contracts against a pinned Alpine root filesystem.
//!
//! These cases preserve the process-facing semantics formerly exercised by the
//! repository scenario group. They live at the public `hl-container` boundary
//! and intentionally do not depend on the retired scenario harness.

use hl_container::{Config, ContainerSpec, Containers, ExitStatus, Guest, Isolation, Process, Sandbox};
use std::{future::Future, path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn process_run_options() -> Result<(), Error> {
    bounded(async {
        let fixture = Fixture::new().await?;
        for case in cases() {
            fixture.run(case).await?;
        }
        fixture.detached().await?;
        fixture.name().await
    })
    .await
}

struct Case {
    id: &'static str,
    process: Process,
    status: ExitStatus,
    stdout: &'static [u8],
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            id: "runflags/env-e",
            process: Process::new("/bin/printenv").args(["FOO"]).env("FOO", "barbaz"),
            status: ExitStatus::Code(0),
            stdout: b"barbaz\n",
        },
        Case {
            id: "runflags/workdir-w",
            process: Process::new("/bin/pwd").working_dir("/var/spool"),
            status: ExitStatus::Code(0),
            stdout: b"/var/spool\n",
        },
        Case {
            id: "runflags/entrypoint",
            process: Process::new("/bin/echo").args(["ENTRYOVERRIDE"]),
            status: ExitStatus::Code(0),
            stdout: b"ENTRYOVERRIDE\n",
        },
        Case {
            id: "runflags/user-uidgid",
            process: Process::new("/usr/bin/id").user(1000, 1000),
            status: ExitStatus::Code(0),
            stdout: b"uid=1000 gid=1000 groups=1000\n",
        },
        Case {
            id: "runflags/exit-code",
            process: Process::new("/bin/sh").args(["-c", "exit 42"]),
            status: ExitStatus::Code(42),
            stdout: b"",
        },
    ]
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

    fn spec(&self, id: &str, process: Process) -> ContainerSpec {
        ContainerSpec::from_directory(&self.rootfs, process)
            .name(id.replace('/', "-"))
            .guest(self.guest)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                network_isolated: true,
                ..Isolation::default()
            })
    }

    async fn run(&self, case: Case) -> Result<(), Error> {
        let name = case.id.replace('/', "-");
        self.containers.create(self.spec(case.id, case.process)).await?;
        self.containers.start(&name).await?;
        let status = self.containers.wait(&name).await?;
        let logs = self.containers.logs(&name).await?;
        self.containers.remove(&name).await?;
        require(
            status == case.status && logs.stdout == case.stdout && logs.stderr.is_empty(),
            format!(
                "{} mismatch: status={status:?} stdout={:?} stderr={:?}",
                case.id,
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            ),
        )
    }

    async fn detached(&self) -> Result<(), Error> {
        let id = "runflags/detached-d";
        let name = id.replace('/', "-");
        self.containers
            .create(self.spec(id, Process::new("/bin/sleep").args(["30"])))
            .await?;
        self.containers.start(&name).await?;
        let active = self.containers.inspect(&name).await?.state.is_active();
        self.containers.remove_force(&name).await?;
        require(active, format!("{id}: container was not active after start"))
    }

    async fn name(&self) -> Result<(), Error> {
        let id = "runflags/name";
        let name = "rf-named";
        self.containers
            .create(self.spec(name, Process::new("/bin/sleep").args(["30"])))
            .await?;
        let preserved = self.containers.inspect(name).await?.spec.name.as_deref() == Some(name);
        self.containers.remove_force(name).await?;
        require(preserved, format!("{id}: configured name was not preserved"))
    }
}

async fn bounded<T>(future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    tokio::time::timeout(Duration::from_secs(45), future)
        .await
        .map_err(|_| "run-option contracts exceeded 45 seconds")?
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

fn require(condition: bool, message: String) -> Result<(), Error> {
    if condition { Ok(()) } else { Err(message.into()) }
}
