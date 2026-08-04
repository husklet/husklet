//! Headless attach session with durable output journal.

use crate::api::support::require;
use hl_container::{Console, ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox};
use std::path::Path;

pub(crate) async fn run(
    containers: &Containers,
    rootfs: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    containers
        .create(
            ContainerSpec::from_directory(
                rootfs,
                Process::new("/bin/sh")
                    .args([
                        "-c",
                        "read line; printf 'headless:%s:%s:%s\\n' \"$HL_TEST\" \"$PWD\" \"$line\"; printf 'headless-error\\n' >&2; exit 17",
                    ])
                    .env("HL_TEST", "alpine")
                    .working_dir("/tmp")
                    .console(Console {
                        stdin: true,
                        terminal: None,
                    }),
            )
            .name("headless-runtime")
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            }),
        )
        .await?;
    let mut session = containers.attach("headless-runtime").await?;
    containers.start("headless-runtime").await?;
    session.write(b"input\n".to_vec()).await?;
    session.close().await;
    session.close().await;
    require(
        session.write(b"late".to_vec()).await.is_err(),
        "closed stdin accepted another write",
    )?;
    require(
        containers.wait("headless-runtime").await? == ExitStatus::Code(17),
        "headless Alpine process returned the wrong exit status",
    )?;
    let logs = containers.logs("headless-runtime").await?;
    if logs.stdout != b"headless:alpine:/tmp:input\n" || logs.stderr != b"headless-error\n" {
        return Err(format!(
            "headless Alpine output mismatch: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        )
        .into());
    }
    let first = session.next().await?.ok_or("live stdout record missing")?;
    let second = session.next().await?.ok_or("live stderr record missing")?;
    let entries = [&first, &second];
    require(
        first.sequence == 1
            && second.sequence == 2
            && entries.iter().any(|entry| {
                entry.stream == hl_container::Stream::Stdout && entry.bytes == logs.stdout
            })
            && entries.iter().any(|entry| {
                entry.stream == hl_container::Stream::Stderr && entry.bytes == logs.stderr
            })
            && session.next().await?.is_none(),
        "live session did not preserve the durable output journal",
    )?;
    Ok(())
}
