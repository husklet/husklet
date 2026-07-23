//! Guest process compatibility cases.

use crate::report::LegacyBatch;
use hl_container::{
    ContainerSpec, Containers, ExecSpec, ExecState, ExitStatus, Isolation, Process, Sandbox,
};
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

const IDS: [&str; 15] = [
    "process/env-multiple",
    "process/env-passthrough",
    "process/exec-env",
    "process/exec-into-running",
    "process/exec-sees-shared-fs",
    "process/exit-nonzero",
    "process/exit-rc-check",
    "process/exit-zero",
    "process/hostname-flag",
    "process/pid1-is-init",
    "process/sigterm-clean-stop",
    "process/stdout-stderr-split",
    "process/uid-root",
    "process/workdir",
    "process/workdir-created",
];

pub(crate) fn group() -> crate::contract::Group {
    crate::contract::Group::new(
        "process",
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
    let mut reports = LegacyBatch::new("process")?;
    for case in Case::all() {
        let id = case.id();
        let scenario = &scenarios[id];
        let Some(attempt) = reports.begin(scenario)? else {
            println!("RESUME {id}");
            continue;
        };
        let result = case.run(containers, rootfs).await;
        reports.complete(scenario, attempt, &result)?;
        result?;
    }
    let signal = &scenarios["process/sigterm-clean-stop"];
    if let Some(attempt) = reports.begin(signal)? {
        let result = Signal.run(containers, rootfs).await;
        reports.complete(signal, attempt, &result)?;
        result?;
    }
    let mut execs = Vec::new();
    for id in [
        "process/exec-env",
        "process/exec-into-running",
        "process/exec-sees-shared-fs",
    ] {
        let scenario = &scenarios[id];
        if let Some(attempt) = reports.begin(scenario)? {
            execs.push((scenario, attempt));
        }
    }
    if !execs.is_empty() {
        let result = Exec.run(containers, rootfs).await;
        for (scenario, attempt) in execs {
            reports.complete(scenario, attempt, &result)?;
        }
        result?;
    }
    reports.finish(Vec::new())?;
    Ok(())
}

struct Case {
    name: &'static str,
    process: Process,
    status: ExitStatus,
    stdout: &'static [u8],
    stderr: &'static [u8],
    hostname: Option<&'static str>,
}

impl Case {
    fn id(&self) -> &'static str {
        match self.name {
            "process-env-passthrough" => "process/env-passthrough",
            "process-env" => "process/env-multiple",
            "process-workdir" => "process/workdir",
            "process-workdir-created" => "process/workdir-created",
            "process-exit-zero" => "process/exit-zero",
            "process-exit-nonzero" => "process/exit-nonzero",
            "process-exit-rc" => "process/exit-rc-check",
            "process-streams" => "process/stdout-stderr-split",
            "process-pid" => "process/pid1-is-init",
            "process-uid" => "process/uid-root",
            "process-hostname" => "process/hostname-flag",
            _ => unreachable!(),
        }
    }
    fn all() -> Vec<Self> {
        vec![
            Self::new(
                "process-env-passthrough",
                Process::new("/bin/printenv")
                    .args(["HL_ENV"])
                    .env("HL_ENV", "hello123"),
            )
            .stdout(b"hello123\n"),
            Self::shell("process-env", "printf '%s' \"$A-$B\"")
                .env("A", "1")
                .env("B", "2")
                .stdout(b"1-2"),
            Self::new("process-workdir", Process::new("/bin/pwd"))
                .working_dir("/etc")
                .stdout(b"/etc\n"),
            Self::new("process-workdir-created", Process::new("/bin/pwd"))
                .working_dir("/made/here")
                .stdout(b"/made/here\n"),
            Self::new("process-exit-zero", Process::new("/bin/true")),
            Self::shell("process-exit-nonzero", "exit 7").status(ExitStatus::Code(7)),
            Self::shell("process-exit-rc", "exit 5").status(ExitStatus::Code(5)),
            Self::shell(
                "process-streams",
                "printf 'OUTLINE\\n'; printf 'ERRLINE\\n' >&2",
            )
            .stdout(b"OUTLINE\n")
            .stderr(b"ERRLINE\n"),
            Self::shell("process-pid", "printf 'PID=%s\\n' \"$$\"").stdout(b"PID=1\n"),
            Self::new("process-uid", Process::new("/usr/bin/id").args(["-u"])).stdout(b"0\n"),
            Self::new("process-hostname", Process::new("/bin/hostname"))
                .hostname("hlbox")
                .stdout(b"hlbox\n"),
        ]
    }

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

    async fn run(self, containers: &Containers, rootfs: &Path) -> Result<(), Error> {
        let mut spec = ContainerSpec::from_directory(rootfs, self.process)
            .name(self.name)
            .isolation(isolation());
        if let Some(hostname) = self.hostname {
            spec = spec.hostname(hostname);
        }
        containers.create(spec).await?;
        containers.start(self.name).await?;
        let status = containers.wait(self.name).await?;
        let logs = containers.logs(self.name).await?;
        containers.remove(self.name).await?;
        if status != self.status || logs.stdout != self.stdout || logs.stderr != self.stderr {
            return Err(format!(
                "{} mismatch: status={status:?} stdout={:?} stderr={:?}",
                self.name,
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            )
            .into());
        }
        Ok(())
    }
}

struct Signal;

impl Signal {
    async fn run(self, containers: &Containers, rootfs: &Path) -> Result<(), Error> {
        let name = "process-signal";
        let process = Process::new("/bin/sh").args([
            "-c",
            "trap 'printf GOT_TERM; exit 0' TERM; printf READY; while :; do :; done",
        ]);
        containers
            .create(
                ContainerSpec::from_directory(rootfs, process)
                    .name(name)
                    .isolation(isolation()),
            )
            .await?;
        containers.start(name).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if containers.logs(name).await?.stdout == b"READY" {
                    return Ok::<_, hl_container::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let status = containers.stop(name, Duration::from_secs(5)).await?;
        let logs = containers.logs(name).await?;
        containers.remove(name).await?;
        if status != ExitStatus::Code(0) || logs.stdout != b"READYGOT_TERM" {
            return Err(format!(
                "clean stop mismatch: status={status:?} stdout={:?}",
                String::from_utf8_lossy(&logs.stdout)
            )
            .into());
        }
        Ok(())
    }
}

struct Exec;

impl Exec {
    async fn run(self, containers: &Containers, rootfs: &Path) -> Result<(), Error> {
        let name = "process-exec";
        containers
            .create(
                ContainerSpec::from_directory(rootfs, Process::new("/bin/sleep").args(["30"]))
                    .name(name)
                    .isolation(isolation()),
            )
            .await?;
        containers.start(name).await?;
        self.check(
            containers,
            name,
            Process::new("/bin/echo").args(["EXEC_OK"]),
            b"EXEC_OK\n",
        )
        .await?;
        self.check(
            containers,
            name,
            Process::new("/bin/sh")
                .args(["-c", "printf %s \"$EE\""])
                .env("EE", "zz"),
            b"zz",
        )
        .await?;
        self.check(
            containers,
            name,
            Process::new("/bin/sh").args(["-c", "printf shared > /tmp/x"]),
            b"",
        )
        .await?;
        self.check(
            containers,
            name,
            Process::new("/bin/cat").args(["/tmp/x"]),
            b"shared",
        )
        .await?;
        containers.stop(name, Duration::ZERO).await?;
        containers.remove(name).await?;
        Ok(())
    }

    async fn check(
        &self,
        containers: &Containers,
        container: &str,
        process: Process,
        expected: &[u8],
    ) -> Result<(), Error> {
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
}

const fn isolation() -> Isolation {
    Isolation {
        sandbox: Sandbox::Disabled,
        read_only_root: false,
        network_isolated: true,
    }
}
