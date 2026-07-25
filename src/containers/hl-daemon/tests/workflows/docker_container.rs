use super::{fixture, require, Error};
use hl_client::{
    api::Channel,
    model::{
        Attachment, CreateContainer, EventFilter, EventQuery, ExecConfig, ExecStart, RestartPolicy,
        Update,
    },
    Client,
};

pub(super) async fn foreground(client: &Client) -> Result<(), Error> {
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: fixture::IMAGE.into(),
                cmd: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'foreground-out\n'; printf 'foreground-error\n' >&2; exit 17".into(),
                ]),
                attach: Attachment {
                    stdout: true,
                    stderr: true,
                    ..Attachment::default()
                },
                ..CreateContainer::default()
            },
            Some("workflow-foreground"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    require(
        client.containers().wait(&created.id).await?.status_code == 17,
        "foreground-exit",
    )?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    if logs.stdout != b"foreground-out\n" || !logs.stderr.starts_with(b"foreground-error\n") {
        return Err(format!(
            "foreground-output stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        )
        .into());
    }
    super::pass("foreground-output");
    client
        .containers()
        .remove(&created.id, false, false)
        .await?;
    Ok(())
}

pub(super) async fn interactive(client: &Client) -> Result<(), Error> {
    let created = client
        .containers()
        .create(
            &serde_json::from_value(serde_json::json!({
                "Image": fixture::IMAGE,
                "Cmd": [
                    "/bin/sh", "-c",
                    "printf 'ready\n'; read value; printf 'interactive:%s\n' \"$value\""
                ],
                "AttachStdin": true,
                "AttachStdout": true,
                "AttachStderr": true,
                "OpenStdin": true,
                "StdinOnce": true
            }))?,
            Some("workflow-interactive"),
        )
        .await?;
    let mut session = client
        .containers()
        .attach(&created.id, true, true, true)
        .await?;
    client.containers().start(&created.id).await?;
    wait_for_output(client, &created.id, b"ready\n").await?;

    let execution = client
        .executions()
        .create(
            &created.id,
            &ExecConfig {
                attach: Attachment {
                    stdout: true,
                    stderr: true,
                    ..Attachment::default()
                },
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'exec-out\n'; printf 'exec-error\n' >&2; exit 29".into(),
                ],
                ..ExecConfig::default()
            },
        )
        .await?;
    let mut exec = client
        .executions()
        .start(&execution.id, &ExecStart::default())
        .await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(frame) = exec.next().await? {
        match frame.channel() {
            Channel::Stdout => stdout.extend_from_slice(frame.bytes()),
            Channel::Stderr => stderr.extend_from_slice(frame.bytes()),
            Channel::Terminal => return Err("non-terminal exec returned a terminal frame".into()),
        }
    }
    require(
        stdout == b"exec-out\n" && stderr.starts_with(b"exec-error\n"),
        "exec-output",
    )?;
    let inspected = client.executions().inspect(&execution.id).await?;
    require(!inspected.running && inspected.exit_code == 29, "exec-exit")?;

    session.write(b"input\n").await?;
    session.close().await?;
    require(
        client.containers().wait(&created.id).await?.status_code == 0,
        "interactive-exit",
    )?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    require(
        logs.stdout == b"ready\ninteractive:input\n",
        "interactive-output",
    )?;
    client
        .containers()
        .remove(&created.id, false, false)
        .await?;
    Ok(())
}

pub(super) async fn lifecycle(client: &Client) -> Result<(), Error> {
    let created = client
        .containers()
        .create(
            &serde_json::from_value(serde_json::json!({
                "Image": fixture::IMAGE,
                "Cmd": ["/bin/sh", "-c", "read value; printf 'full:%s\n' \"$value\""],
                "AttachStdin": true,
                "AttachStdout": true,
                "AttachStderr": true,
                "OpenStdin": true,
                "StdinOnce": true
            }))?,
            Some("workflow-full"),
        )
        .await?;
    let updated = client
        .containers()
        .update(
            &created.id,
            &Update {
                restart_policy: Some(RestartPolicy {
                    name: "no".into(),
                    maximum_retry_count: 0,
                }),
                ..Update::default()
            },
        )
        .await?;
    require(updated.warnings.is_empty(), "container-update")?;
    let mut attached = client
        .containers()
        .attach(&created.id, true, true, true)
        .await?;
    client.containers().start(&created.id).await?;
    attached.write(b"attached\n").await?;
    attached.close().await?;
    require(
        client.containers().wait(&created.id).await?.status_code == 0,
        "full-attach-exit",
    )?;
    require(
        client
            .containers()
            .logs(&created.id, true, false)
            .await?
            .stdout
            == b"full:attached\n",
        "full-attach-output",
    )?;
    client
        .containers()
        .remove(&created.id, false, false)
        .await?;

    let until = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?;
    let mut events = client
        .events()
        .subscribe(
            &EventQuery::default()
                .filters(EventFilter::default().container(&created.id))
                .since(0)
                .until(until),
        )
        .await?;
    let mut actions = Vec::new();
    while let Some(event) = events.next().await? {
        actions.push(event.action);
    }
    require(
        ["create", "start", "die", "destroy"]
            .iter()
            .all(|action| actions.iter().any(|actual| actual == action)),
        "container-events",
    )?;
    Ok(())
}

async fn wait_for_output(client: &Client, id: &str, suffix: &[u8]) -> Result<(), Error> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if client
                .containers()
                .logs(id, true, false)
                .await?
                .stdout
                .ends_with(suffix)
            {
                return Ok::<_, hl_client::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}
