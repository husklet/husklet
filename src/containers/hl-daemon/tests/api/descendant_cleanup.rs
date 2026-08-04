//! Descendant process cleanup on container and exec teardown.

use crate::api::support::{alive, containers_for, read_pid, require, unpack, wait_dead};
use hl_container::{ContainerSpec, ExitStatus, Process};
use std::{env, path::PathBuf};
use tempfile::TempDir;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = work.path().join("rootfs");
    let archive = env::var_os("HL_ALPINE_ARCHIVE")
        .map(PathBuf::from)
        .ok_or("HL_ALPINE_ARCHIVE must name the pinned Alpine minirootfs")?;
    unpack(archive, rootfs.clone()).await?;
    let containers = containers_for(work.path()).await?;
    containers
        .create(
            ContainerSpec::from_directory(
                &rootfs,
                Process::new("/bin/sh").args([
                    "-c",
                    "setsid sh -c 'sleep 60 </dev/null >/dev/null 2>&1 & echo $! >/tmp/init-domain.pid'",
                ]),
            )
            .name("descendant-cleanup"),
        )
        .await?;
    containers.start("descendant-cleanup").await?;
    let status = containers.wait("descendant-cleanup").await?;
    containers.remove("descendant-cleanup").await?;
    if status != ExitStatus::Code(0) {
        return Err(format!("cleanup probe exited as {status:?}").into());
    }
    let init_pid = read_pid(&rootfs.join("tmp/init-domain.pid")).await?;
    wait_dead(init_pid, "init descendant").await?;

    containers
        .create(
            ContainerSpec::from_directory(
                &rootfs,
                Process::new("/bin/sh").args(["-c", "while :; do sleep 60; done"]),
            )
            .name("exec-domain"),
        )
        .await?;
    containers.start("exec-domain").await?;
    let execution = containers
        .executions()
        .create(
            "exec-domain",
            hl_container::ExecSpec::new(Process::new("/bin/sh").args([
                "-c",
                "setsid sh -c 'sleep 60 </dev/null >/dev/null 2>&1 & echo $! >/tmp/exec-domain.pid'",
            ])),
        )
        .await?;
    drop(containers.executions().start(&execution.id).await?);
    let exec_pid = read_pid(&rootfs.join("tmp/exec-domain.pid")).await?;
    require(
        alive(exec_pid).await?,
        "exec descendant exited with its leader",
    )?;
    containers.remove_force("exec-domain").await?;
    wait_dead(exec_pid, "exec descendant").await
}
