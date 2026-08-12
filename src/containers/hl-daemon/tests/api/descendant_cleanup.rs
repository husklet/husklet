//! Descendant process cleanup on container and exec teardown.

use crate::api::support::{containers_for, unpack, wait_changing, wait_stopped};
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
                    ": >/tmp/init-domain.heartbeat; setsid sh -c 'while :; do printf x >>/tmp/init-domain.heartbeat; sleep 0.05; done' </dev/null >/dev/null 2>&1 & while [ ! -s /tmp/init-domain.heartbeat ]; do sleep 0.01; done",
                ]),
            )
            .name("descendant-cleanup"),
        )
        .await?;
    containers.start("descendant-cleanup").await?;
    let heartbeat = rootfs.join("tmp/init-domain.heartbeat");
    wait_changing(&heartbeat, "init descendant").await?;
    let status = containers.wait("descendant-cleanup").await?;
    containers.remove("descendant-cleanup").await?;
    if status != ExitStatus::Code(0) {
        return Err(format!("cleanup probe exited as {status:?}").into());
    }
    wait_stopped(&heartbeat, "init descendant").await?;

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
                ": >/tmp/exec-domain.heartbeat; setsid sh -c 'while :; do printf x >>/tmp/exec-domain.heartbeat; sleep 0.05; done' </dev/null >/dev/null 2>&1 & while [ ! -s /tmp/exec-domain.heartbeat ]; do sleep 0.01; done",
            ])),
        )
        .await?;
    drop(containers.executions().start(&execution.id).await?);
    let heartbeat = rootfs.join("tmp/exec-domain.heartbeat");
    wait_changing(&heartbeat, "exec descendant").await?;
    containers.remove_force("exec-domain").await?;
    wait_stopped(&heartbeat, "exec descendant").await
}
