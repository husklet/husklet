//! Daemon-mediated attach, logs, and exec against a live socket.

use crate::api::support::{require, wait_for_path, TIMEOUT};
use hl_client::{
    api::Channel,
    model::{Attachment, ExecConfig, ExecStart, LogOptions, LogStreams},
    Client,
};
use hl_container::{Console, ContainerSpec, Containers, Isolation, Process, Sandbox};
use hl_daemon::Daemon;
use std::{path::Path, time::Duration};
use tokio::{
    sync::oneshot,
    time::{sleep, timeout},
};

pub(crate) async fn run(
    containers: Containers,
    rootfs: &Path,
    work: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    containers
        .create(
            ContainerSpec::from_directory(
                rootfs,
                Process::new("/bin/sh")
                    .args([
                        "-c",
                        "read line; read shared < /tmp/exec-shared; printf 'daemon:%s:%s:%s\\n' \"$HL_TEST\" \"$line\" \"$shared\"; printf 'daemon-error\\n' >&2; exit 23",
                    ])
                    .env("HL_TEST", "alpine")
                    .console(Console {
                        stdin: true,
                        terminal: None,
                    }),
            )
            .name("daemon-runtime")
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            }),
        )
        .await?;
    log_container(&containers, rootfs).await?;
    let socket = work.join("runtime.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(
        async move {
            let _ = stopped.await;
        },
    ));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;
    logs(&client).await?;
    let mut session = client
        .containers()
        .attach("daemon-runtime", true, true, true)
        .await?;
    client.containers().start("daemon-runtime").await?;
    exec(&client, rootfs).await?;
    session.write(b"input\n").await?;
    session.close().await?;
    require(
        client
            .containers()
            .wait("daemon-runtime")
            .await?
            .status_code
            == 23,
        "daemon Alpine process returned the wrong exit status",
    )?;
    let logs = client
        .containers()
        .logs("daemon-runtime", true, true)
        .await?;
    if logs.stdout != b"daemon:alpine:input:shared\n" || logs.stderr != b"daemon-error\n" {
        return Err(format!(
            "daemon Alpine output mismatch: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        )
        .into());
    }
    let first = session
        .next()
        .await?
        .ok_or("daemon attach stdout missing")?;
    let second = session
        .next()
        .await?
        .ok_or("daemon attach stderr missing")?;
    let frames = [&first, &second];
    require(
        frames.iter().any(|frame| {
            frame.channel() == Channel::Stdout
                && frame.bytes().as_ref() == b"daemon:alpine:input:shared\n"
        }) && frames.iter().any(|frame| {
            frame.channel() == Channel::Stderr && frame.bytes().as_ref() == b"daemon-error\n"
        }) && session.next().await?.is_none(),
        "daemon attach did not preserve both multiplexed streams",
    )?;
    let _ = shutdown.send(());
    server.await??;
    Ok(())
}

async fn log_container(containers: &Containers, rootfs: &Path) -> Result<(), hl_container::Error> {
    let process = Process::new("/bin/sh")
        .args([
            "-c",
            "printf 'history-one\\nhistory-two\\n'; read line; printf 'live:%s\\n' \"$line\"",
        ])
        .console(Console {
            stdin: true,
            terminal: None,
        });
    containers
        .create(
            ContainerSpec::from_directory(rootfs, process)
                .name("daemon-logs")
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    ..Isolation::default()
                }),
        )
        .await?;
    Ok(())
}

async fn logs(client: &Client) -> Result<(), Box<dyn std::error::Error>> {
    let containers = client.containers();
    let mut input = containers.attach("daemon-logs", true, true, false).await?;
    containers.start("daemon-logs").await?;

    timeout(TIMEOUT, async {
        loop {
            let output = containers.logs("daemon-logs", true, false).await?;
            if output.stdout.ends_with(b"history-two\n") {
                return Ok::<_, hl_client::Error>(());
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    let mut logs = containers
        .logs_stream(
            "daemon-logs",
            &LogOptions {
                follow: true,
                streams: LogStreams {
                    stdout: true,
                    stderr: false,
                },
                tail: Some(1),
                ..LogOptions::default()
            },
        )
        .await?;
    input.write(b"release\n").await?;
    input.close().await?;

    let history = timeout(TIMEOUT, logs.next())
        .await??
        .ok_or("followed logs omitted history")?;
    let live = timeout(TIMEOUT, logs.next())
        .await??
        .ok_or("followed logs omitted live output")?;
    require(
        history.channel() == Channel::Stdout
            && history.bytes().as_ref() == b"history-two\n"
            && live.channel() == Channel::Stdout
            && live.bytes().as_ref() == b"live:release\n",
        "logs did not bridge selected tail history into live output",
    )?;
    require(
        containers.wait("daemon-logs").await?.status_code == 0,
        "logs scenario returned a nonzero exit",
    )?;
    require(
        timeout(TIMEOUT, logs.next()).await??.is_none(),
        "logs stream did not close after draining process output",
    )?;
    Ok(())
}

async fn exec(client: &Client, rootfs: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let exec = client
        .executions()
        .create(
            "daemon-runtime",
            &ExecConfig {
                attach: Attachment {
                    stdin: true,
                    stdout: true,
                    stderr: true,
                },
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "read value; printf 'exec:%s:%s\\n' \"$HL_TEST\" \"$value\"; printf 'exec-error\\n' >&2; printf shared > /tmp/exec-shared; exit 29".into(),
                ],
                ..ExecConfig::default()
            },
        )
        .await?;
    let mut execution = client
        .executions()
        .start(&exec.id, &ExecStart::default())
        .await?;
    execution.write(b"value\n").await?;
    execution.close().await?;
    let exec_stdout = execution.next().await?.ok_or("exec stdout missing")?;
    let exec_stderr = execution.next().await?.ok_or("exec stderr missing")?;
    let exec_frames = [&exec_stdout, &exec_stderr];
    require(
        exec_frames.iter().any(|frame| {
            frame.channel() == Channel::Stdout && frame.bytes().as_ref() == b"exec:alpine:value\n"
        }) && exec_frames.iter().any(|frame| {
            frame.channel() == Channel::Stderr && frame.bytes().as_ref() == b"exec-error\n"
        }) && execution.next().await?.is_none(),
        "daemon exec did not preserve exact process output",
    )?;
    let exec_inspect = client.executions().inspect(&exec.id).await?;
    require(
        !exec_inspect.running && exec_inspect.exit_code == 29 && exec_inspect.pid == 0,
        "daemon exec inspection did not preserve the nonzero exit",
    )?;
    require(
        std::fs::read(rootfs.join("tmp/exec-shared"))? == b"shared",
        "daemon exec did not mutate the parent rootfs",
    )?;
    Ok(())
}
