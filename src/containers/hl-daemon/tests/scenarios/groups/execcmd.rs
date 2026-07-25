use crate::report::ScenarioBatch;
use hl_container::{
    ContainerSpec, Containers, ExecSpec, ExecState, ExitStatus, Isolation, Process, Sandbox,
    Stream, Streams,
};
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

const IDS: [&str; 7] = [
    "execcmd/basic",
    "execcmd/env-e",
    "execcmd/workdir-w",
    "execcmd/user-u",
    "execcmd/detached-d",
    "execcmd/exit-code",
    "execcmd/stdin-i",
];

pub(crate) fn group() -> crate::contract::Group {
    crate::contract::Group::new(
        "execcmd",
        IDS.into_iter()
            .map(|id| crate::contract::Scenario::new(id, "alpine:3.20").api(id))
            .collect(),
    )
}

struct Execs<'a> {
    containers: &'a Containers,
    rootfs: &'a Path,
}

impl<'a> Execs<'a> {
    const fn new(containers: &'a Containers, rootfs: &'a Path) -> Self {
        Self { containers, rootfs }
    }

    async fn run(&self) -> Result<(), Error> {
        let scenarios = group()
            .scenarios
            .into_iter()
            .map(|value| (value.id, value))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut reports = ScenarioBatch::new("execcmd")?;
        let mut failures = Vec::new();
        for id in IDS {
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
            "exec command scenarios: {} passed; {} failed; 7 total",
            7 - failures.len(),
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
            "execcmd/basic" => self.basic().await,
            "execcmd/env-e" => self.env().await,
            "execcmd/workdir-w" => self.workdir().await,
            "execcmd/user-u" => self.user().await,
            "execcmd/detached-d" => self.detached().await,
            "execcmd/exit-code" => self.exit().await,
            "execcmd/stdin-i" => self.stdin().await,
            _ => unreachable!(),
        }
    }

    async fn parent(&self, name: &str) -> Result<(), Error> {
        self.containers
            .create(
                ContainerSpec::from_directory(self.rootfs, Process::new("/bin/sleep").args(["60"]))
                    .name(name)
                    .isolation(Isolation {
                        sandbox: Sandbox::Disabled,
                        read_only_root: false,
                        network_isolated: true,
                    }),
            )
            .await?;
        self.containers.start(name).await?;
        Ok(())
    }

    async fn command(&self, parent: &str, spec: ExecSpec) -> Result<(ExitStatus, Vec<u8>), Error> {
        let executions = self.containers.executions();
        let execution = executions.create(parent, spec).await?;
        let mut session = executions.start(&execution.id).await?;
        let mut output = Vec::new();
        while let Some(entry) = session.next().await? {
            if entry.stream == Stream::Stdout {
                output.extend(entry.bytes);
            }
        }
        let execution = executions.inspect(&execution.id).await?;
        match execution.state {
            ExecState::Exited { result, .. } => Ok((result, output)),
            state => Err(format!("execution remained in {state:?}").into()),
        }
    }

    async fn output(&self, name: &str, process: Process, expected: &[u8]) -> Result<(), Error> {
        self.parent(name).await?;
        let result = self.command(name, ExecSpec::new(process)).await;
        let _ = self.containers.stop(name, Duration::ZERO).await;
        let (status, output) = result?;
        if status == ExitStatus::Code(0) && output == expected {
            Ok(())
        } else {
            Err(format!("status={status:?} stdout={output:?}").into())
        }
    }

    async fn basic(&self) -> Result<(), Error> {
        self.output(
            "exec-basic",
            Process::new("/bin/echo").args(["EXECOK"]),
            b"EXECOK\n",
        )
        .await
    }

    async fn env(&self) -> Result<(), Error> {
        self.output(
            "exec-env",
            Process::new("/bin/printenv")
                .args(["XX"])
                .env("XX", "yyval"),
            b"yyval\n",
        )
        .await
    }

    async fn workdir(&self) -> Result<(), Error> {
        self.output(
            "exec-workdir",
            Process::new("/bin/pwd").working_dir("/etc"),
            b"/etc\n",
        )
        .await
    }

    async fn user(&self) -> Result<(), Error> {
        self.parent("exec-user").await?;
        let result = self
            .command(
                "exec-user",
                ExecSpec::new(Process::new("/usr/bin/id").args(["-u"])).user("1000"),
            )
            .await;
        let _ = self.containers.stop("exec-user", Duration::ZERO).await;
        let (status, output) = result?;
        if status == ExitStatus::Code(0) && output == b"1000\n" {
            Ok(())
        } else {
            Err(format!("status={status:?} stdout={output:?}").into())
        }
    }

    async fn detached(&self) -> Result<(), Error> {
        let name = "exec-detached";
        self.parent(name).await?;
        let executions = self.containers.executions();
        let writer = executions
            .create(
                name,
                ExecSpec::new(Process::new("/bin/sh").args(["-c", "echo DETACHEDWROTE > /tmp/d"])),
            )
            .await?;
        drop(executions.start(&writer.id).await?);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let result = self
            .command(
                name,
                ExecSpec::new(Process::new("/bin/cat").args(["/tmp/d"])),
            )
            .await;
        let _ = self.containers.stop(name, Duration::ZERO).await;
        let (status, output) = result?;
        if status == ExitStatus::Code(0) && output == b"DETACHEDWROTE\n" {
            Ok(())
        } else {
            Err(format!("status={status:?} stdout={output:?}").into())
        }
    }

    async fn exit(&self) -> Result<(), Error> {
        let name = "exec-exit";
        self.parent(name).await?;
        let result = self
            .command(
                name,
                ExecSpec::new(Process::new("/bin/sh").args(["-c", "exit 9"])),
            )
            .await;
        let _ = self.containers.stop(name, Duration::ZERO).await;
        let (status, _) = result?;
        if status == ExitStatus::Code(9) {
            Ok(())
        } else {
            Err(format!("status={status:?}; expected exit 9").into())
        }
    }

    async fn stdin(&self) -> Result<(), Error> {
        let name = "exec-stdin";
        self.parent(name).await?;
        let executions = self.containers.executions();
        let execution = executions
            .create(
                name,
                ExecSpec::new(Process::new("/bin/cat")).streams(Streams {
                    stdin: true,
                    stdout: true,
                    stderr: true,
                }),
            )
            .await?;
        let mut session = executions.start(&execution.id).await?;
        session.write(b"INPUTLINE\n".to_vec()).await?;
        session.close().await;
        let mut output = Vec::new();
        while let Some(entry) = session.next().await? {
            if entry.stream == Stream::Stdout {
                output.extend(entry.bytes);
            }
        }
        let _ = self.containers.stop(name, Duration::ZERO).await;
        if output == b"INPUTLINE\n" {
            Ok(())
        } else {
            Err(format!("stdout={output:?}").into())
        }
    }
}

pub(crate) async fn run(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    Execs::new(containers, rootfs).run().await
}
