//! Workspace execution through the standalone container domain.

use crate::config::WorkspaceConfig;
use hl_client::api::Size;
use hl_client::model::{Attachment, ExecAttach, ExecConfig, ExecLifetime, ExecNetwork, ExecStart};
use hl_ws::{Directory, Key, Storage};
use hl_ws_term::PtyBackend;
use std::io;

mod process;

use process::{ExecPty, Output, PaneRuntime, Shell};

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
        if running { Self::Attach } else { Self::Restore }
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PaneLifetime {
    #[default]
    Persisted,
    Live,
    Ephemeral,
}

impl PaneLifetime {
    fn wire(self) -> (ExecLifetime, ExecNetwork, bool) {
        match self {
            Self::Persisted => (ExecLifetime::Persisted, ExecNetwork::Container, false),
            Self::Live => (ExecLifetime::Live, ExecNetwork::Isolated, true),
            Self::Ephemeral => (ExecLifetime::Ephemeral, ExecNetwork::Isolated, true),
        }
    }
}

pub fn launch(
    workspace: &WorkspaceConfig,
    columns: u16,
    rows: u16,
    cwd: Option<&str>,
    slot: Option<&str>,
) -> io::Result<Box<dyn PtyBackend>> {
    launch_with_lifetime(workspace, columns, rows, cwd, slot, PaneLifetime::Persisted)
}

/// Launches an explicitly non-checkpointed pane through the native supervised backend.
///
/// This mode never reads or writes pane reattachment state. Closing it ends the session.
pub fn launch_ephemeral(
    workspace: &WorkspaceConfig,
    columns: u16,
    rows: u16,
    cwd: Option<&str>,
) -> io::Result<Box<dyn PtyBackend>> {
    launch_with_lifetime(workspace, columns, rows, cwd, None, PaneLifetime::Ephemeral)
}

/// Launches a durable live-reattachable pane through the native supervised backend.
///
/// The execution remains addressable while its workspace domain is alive, but deliberately stays outside
/// that domain's checkpoint. Workspace shutdown must therefore refuse while this pane is active.
pub fn launch_live(
    workspace: &WorkspaceConfig,
    columns: u16,
    rows: u16,
    cwd: Option<&str>,
    slot: Option<&str>,
) -> io::Result<Box<dyn PtyBackend>> {
    launch_with_lifetime(workspace, columns, rows, cwd, slot, PaneLifetime::Live)
}

fn launch_with_lifetime(
    workspace: &WorkspaceConfig,
    columns: u16,
    rows: u16,
    cwd: Option<&str>,
    slot: Option<&str>,
    lifetime: PaneLifetime,
) -> io::Result<Box<dyn PtyBackend>> {
    let mut runtime = PaneRuntime::shared()?;
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
    let pane = match lifetime {
        PaneLifetime::Persisted | PaneLifetime::Live => PaneExecution::new(workspace, slot)?,
        PaneLifetime::Ephemeral => None,
    };
    let (exec_lifetime, network, native) = lifetime.wire();
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
        lifetime: exec_lifetime,
        network,
        native,
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
    // What the workspace still has of what this pane remembered. `Some(running)` is the record; `None`
    // is a 404, which for a remembered id always means something the pane had is gone.
    let remembered = match &previous {
        Some(id) => match runtime.block_on(client.executions().inspect(id)) {
            Ok(inspection) => Some(Some(inspection.running)),
            Err(hl_client::Error::Docker { status, .. }) if status.as_u16() == 404 => Some(None),
            Err(error) => return Err(LauncherError::io(error)),
        },
        None => None,
    };
    let mut execution = match PaneStart::resolve(previous.as_deref(), remembered.flatten()) {
        PaneStart::Remembered(action, id) => {
            match action {
                PersistedAction::Attach => restored_running = true,
                PersistedAction::Restore => restoring = true,
            }
            id
        }
        start @ (PaneStart::Fresh | PaneStart::Replaced(_)) => {
            // A replacement must announce itself before the shell that replaces it is seated. Opening
            // a shell is the only way to leave the reader a usable terminal, but a fresh prompt painted
            // over replayed scrollback with no word about the command that was running is
            // indistinguishable from the session having survived.
            restore_failure = start.notice();
            if let (Some(pane), Some(id)) = (&pane, &previous) {
                let _ = pane.clear(id);
            }
            let created = runtime
                .block_on(client.executions().create("workspace", &config))
                .map_err(LauncherError::io)?;
            if let Some(pane) = &pane {
                pane.save(&created.id)?;
            }
            created.id
        }
    };
    let mut attached = None;
    if restored_running {
        attached = reattach(&runtime, &client, &execution, &attach_request)?;
        restoring = attached.is_none();
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
                        restore_failure = Some(notice_line(&format!(
                            "Terminal {slot:?} could not resume its running command ({error}). This is a new shell."
                        )));
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

/// Reattaches to an execution the pane remembers as running.
///
/// `None` means the process exited between the inspect that classified it and this attach, so the
/// pane must restore instead. Reclassifying once here lets the pane recover without asking the
/// reader to reopen the workspace.
fn reattach(
    runtime: &PaneRuntime,
    client: &hl_client::Client,
    execution: &str,
    request: &ExecAttach,
) -> io::Result<Option<hl_client::api::Session>> {
    let attach_error = match runtime.block_on(client.executions().attach(execution, request)) {
        Ok(session) => return Ok(Some(session)),
        Err(error) => error,
    };
    match runtime.block_on(client.executions().inspect(execution)) {
        Ok(inspection) => match PersistedAction::after_failed_attach(Some(inspection.running)) {
            PersistedAction::Restore => Ok(None),
            PersistedAction::Attach => Err(LauncherError::io(attach_error)),
        },
        Err(hl_client::Error::Docker { status, .. }) if status.as_u16() == 404 => {
            debug_assert_eq!(PersistedAction::after_failed_attach(None), PersistedAction::Restore);
            Ok(None)
        }
        Err(error) => Err(LauncherError::io(error)),
    }
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

/// One line of Husklet's own words for the pane, carrying the prefix that attributes it to Husklet and
/// keeps it out of the scrollback a later restore replays.
///
/// Without the prefix the line is ordinary output: the terminal persists it, the next restore replays
/// it as history, and a reader is told about a loss that happened one session ago as though it had just
/// happened now.
fn notice_line(message: &str) -> String {
    format!("{}{message}\r\n", crate::runtime::domain::RESTORE_NOTICE_PREFIX)
}

/// Where a pane starts, decided from what it remembered and what the workspace still has of it.
///
/// The distinction the reader depends on is between the last two: a pane that remembered NOTHING is
/// being opened fresh and owes no explanation, because no command was lost -- none was ever running
/// here. A pane that remembered an id and cannot find it HAS lost something, whatever the reason and
/// whether or not anything else reported it, and seating a new shell in its place without a word is
/// the one response that misrepresents what happened. Both paths open a shell; only one is honest in
/// silence, and the pane can always tell them apart, because it either saved an id or it did not.
#[derive(Debug, Eq, PartialEq)]
enum PaneStart {
    /// No id was remembered. Nothing to resume and nothing to report.
    Fresh,
    /// The remembered execution is still known to the workspace.
    Remembered(PersistedAction, String),
    /// The remembered execution is gone. The words the replacement owes its reader.
    Replaced(String),
}

impl PaneStart {
    /// `previous` is the id this pane saved, and `running` the state of that record -- or `None` when
    /// the workspace no longer has the record at all. A pane that saved no id cannot have lost one,
    /// which is the whole distinction.
    fn resolve(previous: Option<&str>, running: Option<bool>) -> Self {
        match (previous, running) {
            (Some(id), Some(running)) => Self::Remembered(PersistedAction::for_running(running), id.to_owned()),
            (Some(id), None) => Self::Replaced(notice_line(&format!(
                "The program this terminal was running is gone: its session {id} no longer exists in \
                 this workspace. This is a new shell, and it has no memory of what was running here."
            ))),
            (None, _) => Self::Fresh,
        }
    }

    fn notice(self) -> Option<String> {
        match self {
            Self::Replaced(notice) => Some(notice),
            Self::Fresh | Self::Remembered(..) => None,
        }
    }
}

#[cfg(test)]
mod pane_execution_tests {
    use super::{PaneExecution, PaneLifetime, PersistedAction, terminal_identity};
    use crate::config::WorkspaceConfig;
    use hl_client::model::{ExecLifetime, ExecNetwork};
    use hl_ws::Arch;

    #[test]
    fn terminal_defaults_to_the_administrative_workspace_identity() {
        assert_eq!(terminal_identity(), ("0:0", "/root"));
    }

    #[test]
    fn native_pane_modes_request_isolation_without_changing_persisted_defaults() {
        assert_eq!(
            PaneLifetime::Persisted.wire(),
            (ExecLifetime::Persisted, ExecNetwork::Container, false)
        );
        assert_eq!(
            PaneLifetime::Live.wire(),
            (ExecLifetime::Live, ExecNetwork::Isolated, true)
        );
        assert_eq!(
            PaneLifetime::Ephemeral.wire(),
            (ExecLifetime::Ephemeral, ExecNetwork::Isolated, true)
        );
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

    /// A pane that replaces a remembered execution must say so, and must say it in Husklet's own voice.
    ///
    /// The lie this prevents is specific: the workspace restores, the scrollback is replayed, a new
    /// shell is seated at the bottom of it, and the reader sees their own prompt exactly where they left
    /// it -- with no indication that the `sleep 10000` they started is gone. Silence there is not a
    /// missing nicety; it is the product asserting that nothing was lost.
    #[test]
    fn replacing_a_remembered_execution_tells_the_reader_it_is_a_new_shell() {
        let start = super::PaneStart::resolve(Some("exec-42"), None);
        let notice = start.notice().expect("a replaced pane owes its reader an explanation");

        assert!(
            notice.starts_with(crate::runtime::domain::RESTORE_NOTICE_PREFIX),
            "a replacement notice must be attributed to Husklet so the terminal keeps it out of \
             persisted scrollback and never replays it as the reader's own output: {notice:?}"
        );
        assert!(
            notice.contains("exec-42"),
            "the notice must name the session that was lost: {notice:?}"
        );
        assert!(
            notice.contains("new shell"),
            "the notice must say the shell in front of the reader is not the one they left: {notice:?}"
        );
        assert!(
            notice.ends_with("\r\n"),
            "a pane notice terminates a terminal line: {notice:?}"
        );
        assert_eq!(
            super::PaneStart::resolve(None, None).notice(),
            None,
            "a pane opening fresh lost nothing and must not claim it did"
        );
        assert_eq!(
            super::PaneStart::resolve(Some("exec-42"), Some(true)).notice(),
            None,
            "a pane whose remembered execution is still there is resuming, not replacing"
        );
        assert_eq!(
            super::PaneStart::resolve(Some("exec-42"), Some(true)),
            super::PaneStart::Remembered(super::PersistedAction::Attach, "exec-42".to_owned())
        );
        assert_eq!(
            super::PaneStart::resolve(Some("exec-42"), Some(false)),
            super::PaneStart::Remembered(super::PersistedAction::Restore, "exec-42".to_owned())
        );
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
