//! Workspace execution through the standalone container domain.

use crate::config::WorkspaceConfig;
use hl_client::api::Size;
use hl_client::model::{Attachment, ExecConfig, ExecStart};
use hl_ws_term::PtyBackend;
use std::io;

mod process;

use process::{ExecPty, Output, Shell};

struct LauncherError;

impl LauncherError {
    fn io(error: impl std::fmt::Display) -> io::Error {
        io::Error::other(error.to_string())
    }
}

pub fn launch(
    workspace: &WorkspaceConfig,
    columns: u16,
    rows: u16,
    cwd: Option<&str>,
    _slot: Option<&str>,
) -> io::Result<Box<dyn PtyBackend>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let socket = crate::runtime::domain::Domain::new(workspace).ensure(workspace)?;
    let client = hl_client::Client::unix(socket).map_err(LauncherError::io)?;
    runtime
        .block_on(WorkspaceContainer::ready(&client))
        .map_err(LauncherError::io)?;
    let start_dir = cwd
        .map(str::trim)
        .filter(|value| value.starts_with('/') && !value.is_empty())
        .unwrap_or("/root");
    let base = workspace
        .shell
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                "if command -v bash >/dev/null 2>&1; then exec bash -il; else exec sh -i; fi"
                    .to_owned()
            },
            |shell| format!("exec {shell}"),
        );
    let command = format!("cd {} 2>/dev/null; {base}", Shell::quote(start_dir));
    let size = Size::new(rows.max(1), columns.max(1)).map_err(LauncherError::io)?;
    let execution = runtime
        .block_on(client.executions().create(
            "workspace",
            &ExecConfig {
                attach: Attachment {
                    stdin: true,
                    stdout: true,
                    stderr: true,
                },
                tty: true,
                command: vec!["/bin/sh".into(), "-c".into(), command],
                user: "0:0".into(),
                working_dir: start_dir.into(),
                ..ExecConfig::default()
            },
        ))
        .map_err(LauncherError::io)?;
    let session = runtime
        .block_on(client.executions().start(
            &execution.id,
            &ExecStart {
                tty: true,
                kill_on_disconnect: true,
                console_size: Some([u64::from(size.rows()), u64::from(size.columns())]),
                ..ExecStart::default()
            },
        ))
        .map_err(LauncherError::io)?;
    let (input, mut stream) = session.into_terminal().map_err(LauncherError::io)?;

    let (output_tx, output) = std::sync::mpsc::channel();
    let lifecycle_tx = output_tx.clone();
    let exited = std::sync::Arc::new(std::sync::Mutex::new(None));
    runtime.spawn(async move {
        while let Ok(Some(entry)) = stream.next().await {
            if output_tx.send(entry.into_bytes().to_vec()).is_err() {
                return;
            }
        }
    });
    let waiting = client.clone();
    let waiting_id = execution.id.clone();
    let exit = std::sync::Arc::clone(&exited);
    runtime.spawn(async move {
        match waiting.executions().wait(&waiting_id).await {
            Ok(status) => {
                let code = i32::try_from(status.status_code).unwrap_or(70);
                if let Err(error) = waiting.executions().remove(&waiting_id).await {
                    let _ = lifecycle_tx.send(
                        format!("\r\nworkspace execution cleanup failed: {error}\r\n").into_bytes(),
                    );
                }
                *exit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(code);
            }
            Err(error) => {
                let _ = lifecycle_tx
                    .send(format!("\r\nworkspace execution wait failed: {error}\r\n").into_bytes());
                *exit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(70);
            }
        }
    });

    Ok(Box::new(ExecPty {
        runtime,
        client,
        execution: execution.id,
        input,
        output: Output::new(output),
        exited,
    }))
}

struct WorkspaceContainer;

impl WorkspaceContainer {
    async fn ready(client: &hl_client::Client) -> hl_client::Result<()> {
        let container = client.containers().inspect("workspace").await?;
        if container.state.activity.running && !container.state.activity.paused {
            return Ok(());
        }
        match client.containers().start("workspace").await {
            Ok(()) => Ok(()),
            Err(hl_client::Error::Docker { status, .. })
                if matches!(status.as_u16(), 304 | 409) =>
            {
                let container = client.containers().inspect("workspace").await?;
                if container.state.activity.running && !container.state.activity.paused {
                    Ok(())
                } else {
                    Err(hl_client::Error::Protocol(
                        "workspace container did not become ready".into(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }
}
