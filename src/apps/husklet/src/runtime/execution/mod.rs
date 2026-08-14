//! Workspace execution through the standalone container domain.

use crate::config::WorkspaceConfig;
use hl_client::api::Size;
use hl_client::model::{Attachment, ExecConfig, ExecStart};
use hl_ws::{Directory, Key, Storage};
use hl_ws_term::PtyBackend;
use std::io;

mod process;

use process::{ExecPty, Output, Shell};

const PANE_SLOT: &str = "HL_HUSKLET_PANE_SLOT";

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
        env: Some(
            [
                Some(format!("HOME={terminal_home}")),
                slot.map(|slot| format!("{PANE_SLOT}={slot}")),
            ]
            .into_iter()
            .flatten()
            .collect(),
        ),
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
    let mut restore_failure = None;
    let previous = pane.as_ref().map(PaneExecution::id).transpose()?.flatten();
    let restoring = previous.is_some();
    let mut execution = if let Some(id) = previous {
        id
    } else {
        let created = runtime
            .block_on(client.executions().create("workspace", &config))
            .map_err(LauncherError::io)?;
        if let Some(pane) = &pane {
            pane.save(&created.id)?;
        }
        created.id
    };
    let session = match runtime.block_on(client.executions().start(&execution, &start)) {
        Ok(session) => session,
        Err(error) if restoring => {
            restore_failure = Some(format!(
                "workspace restore incomplete: terminal {slot:?} could not resume its running command: {error}; opened a new shell\r\n"
            ));
            if let Some(pane) = &pane {
                let _ = pane.clear(&execution);
            }
            let _ = runtime.block_on(client.executions().remove(&execution));
            let created = runtime
                .block_on(client.executions().create("workspace", &config))
                .map_err(LauncherError::io)?;
            execution = created.id;
            if let Some(pane) = &pane {
                pane.save(&execution)?;
            }
            runtime
                .block_on(client.executions().start(&execution, &start))
                .map_err(LauncherError::io)?
        }
        Err(error) => return Err(LauncherError::io(error)),
    };
    let (input, mut stream) = session.into_terminal().map_err(LauncherError::io)?;

    let (output_tx, output) = std::sync::mpsc::channel();
    if let Some(message) = restore_failure {
        let _ = output_tx.send(message.into_bytes());
    }
    let lifecycle_tx = output_tx.clone();
    let exited = std::sync::Arc::new(std::sync::Mutex::new(None));
    runtime.spawn(async move {
        // Stop forwarding as soon as the stream ends or the reader hangs up.
        let mut open = true;
        while open {
            let Ok(Some(entry)) = stream.next().await else {
                break;
            };
            open = output_tx.send(entry.into_bytes().to_vec()).is_ok();
        }
    });
    let waiting = client.clone();
    let waiting_id = execution.clone();
    let waiting_pane = pane.clone();
    let exit = std::sync::Arc::clone(&exited);
    runtime.spawn(async move {
        let status = match waiting.executions().wait(&waiting_id).await {
            Ok(status) => status,
            Err(error) => {
                let _ = lifecycle_tx.send(format!("\r\nworkspace execution wait failed: {error}\r\n").into_bytes());
                *exit.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(70);
                return;
            }
        };
        let code = i32::try_from(status.status_code).unwrap_or(70);
        if let Err(error) = waiting.executions().remove(&waiting_id).await {
            let _ = lifecycle_tx.send(format!("\r\nworkspace execution cleanup failed: {error}\r\n").into_bytes());
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
        input,
        output: Output::new(output),
        exited,
        pane,
    }))
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
    use super::{terminal_identity, PaneExecution};
    use crate::config::WorkspaceConfig;
    use hl_ws::Arch;

    #[test]
    fn terminal_defaults_to_the_administrative_workspace_identity() {
        assert_eq!(terminal_identity(), ("0:0", "/root"));
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
