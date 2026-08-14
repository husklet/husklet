//! Daemon-mediated attach, logs, and exec against a live socket.

use crate::api::support::{TIMEOUT, require, wait_for_path};
use hl_client::{
    Client,
    api::{AttachOptions, Channel},
    model::{Attachment, ExecConfig, ExecStart, LogOptions, LogStreams},
};
use hl_container::{Console, ContainerSpec, Containers, Isolation, Process, Sandbox};
use hl_daemon::Daemon;
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::oneshot,
    time::{sleep, timeout},
};

pub(crate) async fn run(containers: Containers, rootfs: &Path, work: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;
    logs(&client).await?;
    let mut session = client.containers().attach("daemon-runtime", true, true, true).await?;
    client.containers().start("daemon-runtime").await?;
    terminal_job_control(&client).await?;
    exec(&client, rootfs).await?;
    session.write(b"input\n").await?;
    session.close().await?;
    require(
        client.containers().wait("daemon-runtime").await?.status_code == 23,
        "daemon Alpine process returned the wrong exit status",
    )?;
    let logs = client.containers().logs("daemon-runtime", true, true).await?;
    if logs.stdout != b"daemon:alpine:input:shared\n" || logs.stderr != b"daemon-error\n" {
        return Err(format!(
            "daemon Alpine output mismatch: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        )
        .into());
    }
    let first = session.next().await?.ok_or("daemon attach stdout missing")?;
    let second = session.next().await?.ok_or("daemon attach stderr missing")?;
    let frames = [&first, &second];
    require(
        frames.iter().any(|frame| {
            frame.channel() == Channel::Stdout && frame.bytes().as_ref() == b"daemon:alpine:input:shared\n"
        }) && frames
            .iter()
            .any(|frame| frame.channel() == Channel::Stderr && frame.bytes().as_ref() == b"daemon-error\n")
            && session.next().await?.is_none(),
        "daemon attach did not preserve both multiplexed streams",
    )?;
    let _ = shutdown.send(());
    server.await??;
    Ok(())
}

pub(crate) async fn failed_upgrade(
    containers: Containers,
    rootfs: &Path,
    work: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/sleep").args(["60"]))
                .name("daemon-upgrade")
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    ..Isolation::default()
                }),
        )
        .await?;
    log_container(&containers, rootfs).await?;
    let socket = work.join("upgrade.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;
    let client = Client::unix(&socket)?;
    client.containers().start("daemon-upgrade").await?;
    failed_exec_upgrade_is_cleaned(&client, &socket).await?;
    client.containers().kill("daemon-upgrade", "KILL").await?;
    client.containers().remove("daemon-upgrade", true, false).await?;
    let _ = shutdown.send(());
    server.await??;
    Ok(())
}

async fn failed_exec_upgrade_is_cleaned(client: &Client, socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let exec = client
        .executions()
        .create(
            "daemon-upgrade",
            &ExecConfig {
                attach: Attachment {
                    stdout: true,
                    ..Attachment::default()
                },
                command: vec!["/bin/sleep".into(), "60".into()],
                ..ExecConfig::default()
            },
        )
        .await?;
    let body = br#"{"Detach":false,"Tty":false,"KillOnDisconnect":true}"#;
    let request = format!(
        "POST /v1.43/exec/{}/start HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
        exec.id,
        body.len()
    );
    let mut stream = UnixStream::connect(socket).await?;
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(body).await?;
    let mut response = Vec::new();
    while !response.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let mut bytes = [0_u8; 256];
        let size = timeout(TIMEOUT, stream.read(&mut bytes))
            .await
            .map_err(|_| "exec upgrade response timed out")??;
        require(size != 0, "exec upgrade response closed before its headers")?;
        response.extend_from_slice(&bytes[..size]);
    }
    require(response.starts_with(b"HTTP/1.1 101"), "exec start did not upgrade")?;
    drop(stream);

    timeout(TIMEOUT, async {
        loop {
            match client.executions().inspect(&exec.id).await {
                Err(hl_client::Error::Docker { status, .. }) if status == http::StatusCode::NOT_FOUND => return Ok(()),
                Ok(_) => sleep(Duration::from_millis(10)).await,
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| "exec survived a failed attach upgrade")??;
    Ok(())
}

async fn terminal_job_control(client: &Client) -> Result<(), Box<dyn std::error::Error>> {
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
                tty: true,
                command: vec![
                    "/bin/sh".into(),
                    "-ic".into(),
                    "set -o; printf 'job-control-ready\\n'".into(),
                ],
                ..ExecConfig::default()
            },
        )
        .await?;
    let mut execution = client
        .executions()
        .start(
            &exec.id,
            &ExecStart {
                tty: true,
                console_size: Some([24, 80]),
                ..ExecStart::default()
            },
        )
        .await?;
    execution.close().await?;
    let mut output = Vec::new();
    while let Some(frame) = timeout(TIMEOUT, execution.next()).await?? {
        output.extend_from_slice(frame.bytes());
    }
    let status = client.executions().wait(&exec.id).await?;
    let text = String::from_utf8_lossy(&output);
    require(
        status.status_code == 0,
        &format!("interactive terminal failed with {}: {text:?}", status.status_code),
    )?;
    require(
        text.contains("job-control-ready") && !text.contains("job control turned off"),
        &format!("interactive shell did not own its terminal foreground group: {text:?}"),
    )?;
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
    .await
    .map_err(|_| "timed out waiting for initial attach history")??;

    let history_options = AttachOptions {
        logs: true,
        stdout: true,
        ..AttachOptions::default()
    };
    let mut replay = containers.attach_with("daemon-logs", &history_options).await?;
    let mut replayed = Vec::new();
    while let Some(output) = timeout(TIMEOUT, replay.next())
        .await
        .map_err(|_| "logs-only attach did not terminate")??
    {
        require(
            output.channel() == Channel::Stdout,
            "logs-only attach changed output channel",
        )?;
        replayed.extend_from_slice(output.bytes());
    }
    require(
        replayed == b"history-one\nhistory-two\n",
        "logs-only attach did not stop at its replay boundary",
    )?;

    let mut empty = containers
        .attach_with(
            "daemon-logs",
            &AttachOptions {
                stream: true,
                ..AttachOptions::default()
            },
        )
        .await?;
    require(
        timeout(TIMEOUT, empty.next())
            .await
            .map_err(|_| "empty attach did not terminate")??
            .is_none(),
        "attach with no selected standard streams remained live",
    )?;

    let mut replay_and_follow = containers
        .attach_with(
            "daemon-logs",
            &AttachOptions {
                stream: true,
                ..history_options.clone()
            },
        )
        .await?;
    let mut attached_history = Vec::new();
    while attached_history.len() < replayed.len() {
        let output = timeout(TIMEOUT, replay_and_follow.next())
            .await
            .map_err(|_| "replay-and-follow attach timed out during history")??
            .ok_or("replay-and-follow attach omitted history")?;
        attached_history.extend_from_slice(output.bytes());
    }
    require(
        attached_history == replayed,
        "replay-and-follow attach changed historical output",
    )?;

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
        .await
        .map_err(|_| "logs stream timed out during history")??
        .ok_or("followed logs omitted history")?;
    let live = timeout(TIMEOUT, logs.next())
        .await
        .map_err(|_| "logs stream timed out during live output")??
        .ok_or("followed logs omitted live output")?;
    let attached_live = timeout(TIMEOUT, replay_and_follow.next())
        .await
        .map_err(|_| "replay-and-follow attach timed out during live output")??
        .ok_or("replay-and-follow attach omitted live output")?;
    require(
        history.channel() == Channel::Stdout
            && history.bytes().as_ref() == b"history-two\n"
            && live.channel() == Channel::Stdout
            && live.bytes().as_ref() == b"live:release\n"
            && attached_live.channel() == Channel::Stdout
            && attached_live.bytes().as_ref() == b"live:release\n",
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
    let mut execution = client.executions().start(&exec.id, &ExecStart::default()).await?;
    let running_pid = client.executions().inspect(&exec.id).await?.pid;
    require(running_pid > 0, "running daemon exec did not expose its process ID")?;
    execution.write(b"value\n").await?;
    execution.close().await?;
    let exec_stdout = execution.next().await?.ok_or("exec stdout missing")?;
    let exec_stderr = execution.next().await?.ok_or("exec stderr missing")?;
    let exec_frames = [&exec_stdout, &exec_stderr];
    require(
        exec_frames
            .iter()
            .any(|frame| frame.channel() == Channel::Stdout && frame.bytes().as_ref() == b"exec:alpine:value\n")
            && exec_frames
                .iter()
                .any(|frame| frame.channel() == Channel::Stderr && frame.bytes().as_ref() == b"exec-error\n")
            && execution.next().await?.is_none(),
        "daemon exec did not preserve exact process output",
    )?;
    let exec_inspect = client.executions().inspect(&exec.id).await?;
    require(
        !exec_inspect.running && exec_inspect.exit_code == 29 && exec_inspect.pid == running_pid,
        "daemon exec inspection did not preserve the nonzero exit and process ID",
    )?;
    require(
        std::fs::read(rootfs.join("tmp/exec-shared"))? == b"shared",
        "daemon exec did not mutate the parent rootfs",
    )?;
    Ok(())
}
