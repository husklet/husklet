//! Workspace execution through the standalone container domain.

use crate::config::WorkspaceConfig;
use hl_client::api::Size;
use hl_client::model::{Attachment, ExecAttach, ExecConfig, ExecStart};
use hl_ws::{Directory, Key, Storage};
use hl_ws_term::PtyBackend;
use std::io;

mod process;

use process::{ExecPty, Output, Shell};

#[derive(Clone)]
pub(crate) struct PaneExecution {
    storage: Directory,
    key: Key,
}

impl PaneExecution {
    fn new(workspace: &WorkspaceConfig, slot: Option<&str>) -> io::Result<Option<Self>> {
        let Some(slot) = slot else {
            return Ok(None);
        };
        let storage = Directory::open(workspace.storage_dir(&crate::paths::hl_root())).map_err(LauncherError::io)?;
        let key = Key::parse(format!("state/executions/{slot}")).map_err(LauncherError::io)?;
        Ok(Some(Self { storage, key }))
    }

    fn id(&self) -> io::Result<Option<String>> {
        match self.storage.get(&self.key) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(hl_ws::storage::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(LauncherError::io(error)),
        }
    }

    fn save(&self, id: &str) -> io::Result<()> {
        self.storage.put(&self.key, id.as_bytes()).map_err(LauncherError::io)
    }

    fn clear(&self, id: &str) -> io::Result<()> {
        if self.id()?.as_deref() != Some(id) {
            return Ok(());
        }
        match self.storage.remove(&self.key) {
            Ok(()) => Ok(()),
            Err(hl_ws::storage::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(LauncherError::io(error)),
        }
    }

    pub(crate) fn clear_all(workspace: &WorkspaceConfig) -> io::Result<()> {
        let storage = Directory::open(workspace.storage_dir(&crate::paths::hl_root())).map_err(LauncherError::io)?;
        let prefix = Key::parse("state/executions").map_err(LauncherError::io)?;
        for key in storage.list(Some(&prefix)).map_err(LauncherError::io)? {
            storage.remove(&key).map_err(LauncherError::io)?;
        }
        Ok(())
    }
}

struct LauncherError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistedAction {
    Attach,
    Restore,
}

impl PersistedAction {
    fn for_running(running: bool) -> Self {
        if running {
            Self::Attach
        } else {
            Self::Restore
        }
    }

    fn after_failed_attach(running: Option<bool>) -> Self {
        running.map_or(Self::Restore, Self::for_running)
    }
}

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
    slot: Option<&str>,
) -> io::Result<Box<dyn PtyBackend>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let socket = crate::runtime::domain::Domain::new(workspace).ensure(workspace)?;
    let client = hl_client::Client::unix(socket).map_err(LauncherError::io)?;
    let _workspace_session = runtime
        .block_on(WorkspaceContainer::ready(&client))
        .map_err(LauncherError::io)?;
    // Workspaces currently expose no user-selection control. Match the root identity used to create the
    // workspace container instead of inheriting an OCI image's advisory USER for interactive panes.
    // Otherwise Ubuntu images open as the unprivileged `ubuntu` account and cannot administer their own
    // package database. When user selection becomes public, it must flow through one explicit policy here.
    let (terminal_user, terminal_home) = terminal_identity();
    let base = workspace
        .shell
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || "if command -v bash >/dev/null 2>&1; then exec bash -il; else exec sh -i; fi".to_owned(),
            |shell| format!("exec {shell}"),
        );
    let (working_dir, command) = terminal_start(cwd, terminal_home, &base);
    let size = Size::new(rows.max(1), columns.max(1)).map_err(LauncherError::io)?;
    let pane = PaneExecution::new(workspace, slot)?;
    let config = ExecConfig {
        attach: Attachment {
            stdin: true,
            stdout: true,
            stderr: true,
        },
        tty: true,
        env: Some(vec![format!("HOME={terminal_home}")]),
        command: vec!["/bin/sh".into(), "-c".into(), command],
        user: terminal_user.into(),
        working_dir,
        ..ExecConfig::default()
    };
    let start = ExecStart {
        tty: true,
        kill_on_disconnect: true,
        console_size: Some([u64::from(size.rows()), u64::from(size.columns())]),
        ..ExecStart::default()
    };
    let attach_request = ExecAttach {
        tty: true,
        kill_on_disconnect: true,
        console_size: start.console_size,
    };
    let mut restore_failure = None;
    let previous = pane.as_ref().map(PaneExecution::id).transpose()?.flatten();
    let mut restored_running = false;
    let mut restoring = false;
    let mut execution = if let Some(id) = previous {
        match runtime.block_on(client.executions().inspect(&id)) {
            Ok(inspection) => {
                match PersistedAction::for_running(inspection.running) {
                    PersistedAction::Attach => restored_running = true,
                    PersistedAction::Restore => restoring = true,
                }
                id
            }
            Err(hl_client::Error::Docker { status, .. }) if status.as_u16() == 404 => {
                if let Some(pane) = &pane {
                    let _ = pane.clear(&id);
                }
                let created = runtime
                    .block_on(client.executions().create("workspace", &config))
                    .map_err(LauncherError::io)?;
                if let Some(pane) = &pane {
                    pane.save(&created.id)?;
                }
                created.id
            }
            Err(error) => return Err(LauncherError::io(error)),
        }
    } else {
        let created = runtime
            .block_on(client.executions().create("workspace", &config))
            .map_err(LauncherError::io)?;
        if let Some(pane) = &pane {
            pane.save(&created.id)?;
        }
        created.id
    };
    let mut attached = None;
    if restored_running {
        match runtime.block_on(client.executions().attach(&execution, &attach_request)) {
            Ok(session) => attached = Some(session),
            Err(attach_error) => match runtime.block_on(client.executions().inspect(&execution)) {
                // The process exited between inspect and attach. Reclassify it
                // once in this launch so the pane can recover without asking
                // the user to reopen the workspace again.
                Ok(inspection) => match PersistedAction::after_failed_attach(Some(inspection.running)) {
                    PersistedAction::Restore => restoring = true,
                    PersistedAction::Attach => return Err(LauncherError::io(attach_error)),
                },
                Err(hl_client::Error::Docker { status, .. }) if status.as_u16() == 404 => {
                    debug_assert_eq!(PersistedAction::after_failed_attach(None), PersistedAction::Restore);
                    restoring = true;
                }
                Err(error) => return Err(LauncherError::io(error)),
            },
        }
    }
    let session = if let Some(session) = attached {
        session
    } else {
        match runtime.block_on(client.executions().start(&execution, &start)) {
            Ok(session) => session,
            Err(error) if restoring => {
                // Reclassify immediately before replacement. A restored exec
                // can become Running between the earlier inspect and this
                // failed start; attaching it is always preferable to
                // orphaning it and creating a second shell.
                let running = match runtime.block_on(client.executions().inspect(&execution)) {
                    Ok(inspection) => inspection.running,
                    Err(hl_client::Error::Docker { status, .. }) if status.as_u16() == 404 => false,
                    Err(inspect) => return Err(LauncherError::io(inspect)),
                };
                if running {
                    runtime
                        .block_on(client.executions().attach(&execution, &attach_request))
                        .map_err(LauncherError::io)?
                } else {
                    let created = runtime
                        .block_on(client.executions().create("workspace", &config))
                        .map_err(LauncherError::io)?;
                    if let Some(pane) = &pane {
                        if let Err(save) = pane.save(&created.id) {
                            let _ = runtime.block_on(client.executions().remove(&created.id));
                            return Err(save);
                        }
                    }
                    let raced_attachment = match runtime.block_on(client.executions().remove(&execution)) {
                        Ok(()) => None,
                        Err(hl_client::Error::Docker { status, .. }) if status.as_u16() == 404 => None,
                        Err(remove) => {
                            if let Some(pane) = &pane {
                                let _ = pane.save(&execution);
                            }
                            let _ = runtime.block_on(client.executions().remove(&created.id));
                            match runtime.block_on(client.executions().inspect(&execution)) {
                                Ok(inspection) if inspection.running => Some(
                                    runtime
                                        .block_on(client.executions().attach(&execution, &attach_request))
                                        .map_err(LauncherError::io)?,
                                ),
                                _ => return Err(LauncherError::io(remove)),
                            }
                        }
                    };
                    if let Some(session) = raced_attachment {
                        session
                    } else {
                        execution = created.id;
                        restore_failure = Some(format!(
                            "workspace restore incomplete: terminal {slot:?} could not resume its running command: {error}; opened a new shell\r\n"
                        ));
                        runtime
                            .block_on(client.executions().start(&execution, &start))
                            .map_err(LauncherError::io)?
                    }
                }
            }
            Err(error) => return Err(LauncherError::io(error)),
        }
    };
    let (mut input, stream) = session.into_terminal().map_err(LauncherError::io)?;

    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(process::OUTPUT_QUEUE_RECORDS);
    runtime.spawn(async move {
        while let Some(bytes) = input_rx.recv().await {
            if input.write(&bytes).await.is_err() {
                break;
            }
        }
    });

    let (output_tx, output) = tokio::sync::mpsc::channel(process::OUTPUT_QUEUE_RECORDS);
    if let Some(message) = restore_failure {
        let _ = output_tx.try_send(message.into_bytes());
    }
    let lifecycle_tx = output_tx.clone();
    let exited = std::sync::Arc::new(std::sync::Mutex::new(None));
    let streaming = client.clone();
    let streaming_id = execution.clone();
    runtime.spawn(forward_output(stream, output_tx, streaming, streaming_id));
    let waiting = client.clone();
    let waiting_id = execution.clone();
    let waiting_pane = pane.clone();
    let exit = std::sync::Arc::clone(&exited);
    runtime.spawn(async move {
        let status = match waiting.executions().wait(&waiting_id).await {
            Ok(status) => status,
            Err(error) => {
                let _ = lifecycle_tx
                    .send(format!("\r\nworkspace execution wait failed: {error}\r\n").into_bytes())
                    .await;
                *exit.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(70);
                return;
            }
        };
        let code = i32::try_from(status.status_code).unwrap_or(70);
        if let Err(error) = waiting.executions().remove(&waiting_id).await {
            let _ = lifecycle_tx
                .send(format!("\r\nworkspace execution cleanup failed: {error}\r\n").into_bytes())
                .await;
        }
        if let Some(pane) = waiting_pane {
            let _ = pane.clear(&waiting_id);
        }
        *exit.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(code);
    });

    Ok(Box::new(ExecPty {
        runtime,
        client,
        execution,
        input: input_tx,
        output: Output::new(output),
        exited,
        pane,
    }))
}

async fn forward_output(
    mut stream: hl_client::api::TerminalOutput,
    output: tokio::sync::mpsc::Sender<Vec<u8>>,
    client: hl_client::Client,
    execution: String,
) {
    // Stop forwarding as soon as the stream ends or the reader hangs up.
    let mut open = true;
    while open {
        match stream.next().await {
            Ok(Some(entry)) => open = output.send(entry.into_bytes().to_vec()).await.is_ok(),
            Ok(None) => break,
            Err(error) => {
                let _ = output
                    .send(format!("\r\nworkspace terminal transport failed: {error}\r\n").into_bytes())
                    .await;
                break;
            }
        }
    }
    // The attachment owns this interactive execution (`kill_on_disconnect`). Explicitly request
    // termination as well so a broken transport cannot leave the wait task and GUI pane hanging if
    // the remote disconnect path itself failed before applying that policy.
    let _ = client.executions().signal(&execution, "KILL").await;
}

fn terminal_identity() -> (&'static str, &'static str) {
    ("0:0", "/root")
}

fn terminal_start(cwd: Option<&str>, home: &str, base: &str) -> (String, String) {
    let requested = cwd
        .map(str::trim)
        .filter(|value| value.starts_with('/') && !value.is_empty())
        .unwrap_or(home);
    (
        home.to_owned(),
        format!(
            "cd {} 2>/dev/null || cd {}; {base}",
            Shell::quote(requested),
            Shell::quote(home)
        ),
    )
}

#[cfg(test)]
mod terminal_start_tests {
    use super::terminal_start;

    #[test]
    fn inherited_directory_is_attempted_from_a_safe_home_baseline() {
        let (working_dir, command) = terminal_start(Some(" /tmp/deleted "), "/root", "exec bash -il");
        assert_eq!(working_dir, "/root");
        assert_eq!(command, "cd '/tmp/deleted' 2>/dev/null || cd '/root'; exec bash -il");
    }

    #[test]
    fn inherited_directory_is_shell_quoted_and_relative_values_are_ignored() {
        let (working_dir, command) = terminal_start(Some("/tmp/a'b; echo unsafe"), "/root", "exec bash -il");
        assert_eq!(working_dir, "/root");
        assert_eq!(
            command,
            "cd '/tmp/a'\\''b; echo unsafe' 2>/dev/null || cd '/root'; exec bash -il"
        );

        let (_, command) = terminal_start(Some("tmp/relative"), "/root", "exec bash -il");
        assert_eq!(command, "cd '/root' 2>/dev/null || cd '/root'; exec bash -il");
    }
}

struct WorkspaceContainer;

impl WorkspaceContainer {
    async fn ready(client: &hl_client::Client) -> io::Result<crate::runtime::session::Session> {
        let container = client
            .containers()
            .inspect("workspace")
            .await
            .map_err(io::Error::other)?;
        if container.state.activity.running && !container.state.activity.paused {
            return crate::runtime::session::Session::from_labels(&container.config.labels);
        }
        match client.containers().start("workspace").await {
            Ok(()) => {
                let container = client
                    .containers()
                    .inspect("workspace")
                    .await
                    .map_err(io::Error::other)?;
                crate::runtime::session::Session::from_labels(&container.config.labels)
            }
            Err(hl_client::Error::Docker { status, .. }) if matches!(status.as_u16(), 304 | 409) => {
                let container = client
                    .containers()
                    .inspect("workspace")
                    .await
                    .map_err(io::Error::other)?;
                if container.state.activity.running && !container.state.activity.paused {
                    crate::runtime::session::Session::from_labels(&container.config.labels)
                } else {
                    Err(io::Error::other("workspace container did not become ready"))
                }
            }
            Err(error) => Err(io::Error::other(error)),
        }
    }
}

#[cfg(test)]
mod pane_execution_tests {
    use super::{terminal_identity, PaneExecution, PersistedAction};
    use crate::config::WorkspaceConfig;
    use hl_ws::Arch;

    #[test]
    fn terminal_defaults_to_the_administrative_workspace_identity() {
        assert_eq!(terminal_identity(), ("0:0", "/root"));
    }

    #[test]
    fn restored_running_panes_attach_while_created_panes_resume() {
        assert_eq!(PersistedAction::for_running(true), PersistedAction::Attach);
        assert_eq!(PersistedAction::for_running(false), PersistedAction::Restore);
        assert_eq!(
            PersistedAction::after_failed_attach(Some(true)),
            PersistedAction::Attach
        );
        assert_eq!(
            PersistedAction::after_failed_attach(Some(false)),
            PersistedAction::Restore
        );
        assert_eq!(PersistedAction::after_failed_attach(None), PersistedAction::Restore);
    }

    #[test]
    fn pane_execution_identity_survives_reopen_and_is_cleared_by_its_owner_only() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
        workspace.storage = Some(temporary.path().to_owned());
        let pane = PaneExecution::new(&workspace, Some("pane-3")).unwrap().unwrap();

        assert_eq!(pane.id().unwrap(), None);
        pane.save("exec-one").unwrap();
        assert_eq!(
            PaneExecution::new(&workspace, Some("pane-3"))
                .unwrap()
                .unwrap()
                .id()
                .unwrap()
                .as_deref(),
            Some("exec-one")
        );
        pane.clear("different-exec").unwrap();
        assert_eq!(pane.id().unwrap().as_deref(), Some("exec-one"));
        let other = PaneExecution::new(&workspace, Some("pane-4")).unwrap().unwrap();
        other.save("exec-two").unwrap();
        PaneExecution::clear_all(&workspace).unwrap();
        assert_eq!(pane.id().unwrap(), None);
        assert_eq!(other.id().unwrap(), None);
    }
}
