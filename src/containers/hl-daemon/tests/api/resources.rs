//! Guest resource compatibility cases.

use hl_container::{
    ContainerSpec, Containers, ExitStatus, Isolation, Logs, Process, Resources, Sandbox,
};
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

pub(crate) async fn run(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    limits(containers, rootfs).await?;
    processes(containers, rootfs).await
}

async fn limits(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    let case = Case {
        name: "resource-limits",
        command: "echo MEM:$(cat /sys/fs/cgroup/memory.max); echo PIDS:$(cat /sys/fs/cgroup/pids.max); echo CPU:$(cat /sys/fs/cgroup/cpu.max); echo ONLINE:$(getconf _NPROCESSORS_ONLN); if awk 'BEGIN { data=sprintf(\"%40000000s\", \"\"); print length(data) }' >/dev/null 2>&1; then echo OOM:0; else echo OOM:1; fi",
        resources: Resources {
            memory_bytes: 32 * 1024 * 1024,
            process_count: 16,
            cpu_count: 2,
        },
        timeout: Duration::from_secs(15),
    };
    let outcome = case.run(containers, rootfs).await?;
    let output = String::from_utf8(outcome.logs.stdout)?;
    if outcome.exit != ExitStatus::Code(0) {
        return Err(format!(
            "resource visibility exited with {:?}: {output:?}",
            outcome.exit
        )
        .into());
    }
    for expected in [
        "MEM:33554432",
        "PIDS:16",
        "CPU:200000 100000",
        "ONLINE:2",
        "OOM:1",
    ] {
        if !output.lines().any(|line| line == expected) {
            return Err(format!("resource output omitted {expected:?}: {output:?}").into());
        }
    }
    Ok(())
}

async fn processes(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    let case = Case {
        name: "process-limit",
        command: "echo PIDS:$(cat /sys/fs/cgroup/pids.max); i=0; while [ $i -lt 10 ]; do sleep 1 & i=$((i+1)); done; wait",
        resources: Resources {
            process_count: 4,
            ..Resources::default()
        },
        timeout: Duration::from_secs(8),
    };
    let outcome = case.run(containers, rootfs).await?;
    let stdout = String::from_utf8(outcome.logs.stdout)?;
    let stderr = String::from_utf8(outcome.logs.stderr)?;
    if outcome.exit == ExitStatus::Code(0)
        || !stdout.lines().any(|line| line == "PIDS:4")
        || !stderr.contains("Resource temporarily unavailable")
    {
        return Err(format!(
            "process limit was not enforced: exit={:?} stdout={stdout:?} stderr={stderr:?}",
            outcome.exit
        )
        .into());
    }
    Ok(())
}

struct Case<'a> {
    name: &'a str,
    command: &'a str,
    resources: Resources,
    timeout: Duration,
}

struct Outcome {
    exit: ExitStatus,
    logs: Logs,
}

impl Case<'_> {
    async fn run(&self, containers: &Containers, rootfs: &Path) -> Result<Outcome, Error> {
        let container = containers.create(self.spec(rootfs)).await?;
        containers.start(container.id.as_str()).await?;
        let waited =
            tokio::time::timeout(self.timeout, containers.wait(container.id.as_str())).await;
        let exit = if let Ok(exit) = waited {
            exit?
        } else {
            let logs = containers.logs(container.id.as_str()).await?;
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                containers.stop(container.id.as_str(), Duration::ZERO),
            )
            .await;
            return Err(format!(
                "{} timed out: stdout={:?} stderr={:?}",
                self.name,
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            )
            .into());
        };
        let logs = containers.logs(container.id.as_str()).await?;
        Ok(Outcome { exit, logs })
    }

    fn spec(&self, rootfs: &Path) -> ContainerSpec {
        ContainerSpec::from_directory(rootfs, Process::new("/bin/sh").args(["-c", self.command]))
            .name(self.name)
            .resources(self.resources)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            })
    }
}
