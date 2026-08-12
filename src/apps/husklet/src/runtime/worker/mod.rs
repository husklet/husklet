//! Private workspace worker used by the signed application.

use crate::config::WorkspaceStore;
use crate::paths;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

mod terminal;

use terminal::{ControllingTerminal, OpenFiles, TerminalSession};

static WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_signal: libc::c_int) {
    WINCH.store(true, Ordering::SeqCst);
}

/// Process-isolated operations used by terminal panes and workspace resource views.
pub struct Worker;

/// Process exit codes used when the worker cannot start a guest session.
///
/// These codes are diagnostic only. Once a guest starts, its exit code is
/// forwarded unchanged and may have the same numeric value.
struct Status;

impl Status {
    pub const TERMINAL_UNAVAILABLE: i32 = 70;
    pub const DESCRIPTORS_UNAVAILABLE: i32 = 71;
    pub const WORKSPACE_MISSING: i32 = 72;
    pub const LAUNCH_FAILED: i32 = 73;
    pub const INVALID_ENGINE_STATUS: i32 = 74;
}

struct Diagnostics(Option<PathBuf>);

struct ProcessStatus(i32);

impl ProcessStatus {
    fn from_engine(status: i32) -> Self {
        Self(if (0..=255).contains(&status) {
            status
        } else {
            Status::INVALID_ENGINE_STATUS
        })
    }
}

impl Diagnostics {
    fn new(path: Option<&Path>) -> Self {
        Self(path.map(Path::to_owned))
    }

    fn record(&self, message: impl std::fmt::Display) {
        let Some(path) = &self.0 else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{message}");
        }
    }
}

impl Worker {
    pub fn launch(name: &str, cwd: Option<&str>, slot: Option<&str>, diagnostics: Option<&Path>) -> ! {
        let diagnostics = Diagnostics::new(diagnostics);
        diagnostics.record(format_args!("worker pid={} starting", std::process::id()));
        if let Err(error) = ControllingTerminal::claim() {
            diagnostics.record(format_args!("controlling terminal failed: {error}"));
            eprintln!("workspace terminal unavailable: {error}");
            std::process::exit(Status::TERMINAL_UNAVAILABLE);
        }
        if let Err(error) = OpenFiles::prepare() {
            diagnostics.record(format_args!("descriptor capacity failed: {error}"));
            eprintln!("workspace descriptor capacity unavailable: {error}");
            std::process::exit(Status::DESCRIPTORS_UNAVAILABLE);
        }
        diagnostics.record(terminal::contract());
        let store = match WorkspaceStore::load(Self::store()) {
            Ok(store) => store,
            Err(error) => {
                diagnostics.record(format_args!("workspace configuration failed: {error}"));
                eprintln!("workspace configuration unavailable: {error}");
                std::process::exit(Status::WORKSPACE_MISSING);
            }
        };
        let Some(workspace) = store.get_key(name).cloned() else {
            eprintln!("workspace key {name:?} does not exist");
            std::process::exit(Status::WORKSPACE_MISSING);
        };
        let (columns, rows) = terminal::size().unwrap_or((80, 24));
        let mut terminal = match crate::runtime::execution::launch(&workspace, columns, rows, cwd, slot) {
            Ok(terminal) => terminal,
            Err(error) => {
                diagnostics.record(format_args!("workspace launch failed: {error}"));
                eprintln!("workspace launch failed: {error}");
                std::process::exit(Status::LAUNCH_FAILED);
            }
        };
        match crate::runtime::domain::Domain::take_restore_summary(&workspace) {
            Ok(Some(summary)) => eprintln!("{summary}"),
            Ok(None) => {}
            Err(error) => eprintln!("workspace restore summary unavailable: {error}"),
        }
        diagnostics.record("workspace launch started");
        let status = TerminalSession::run(&mut *terminal);
        diagnostics.record(format_args!("workspace terminal exited: {status}"));
        std::process::exit(ProcessStatus::from_engine(status).0);
    }

    pub fn daemon(name: &str) -> std::io::Result<std::path::PathBuf> {
        let store = WorkspaceStore::load(Self::store())?;
        let workspace = store.get_key(name).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("workspace key {name:?} does not exist"),
            )
        })?;
        crate::runtime::resources::Daemon::new(workspace).ensure()
    }

    pub fn domain(name: &str) -> std::io::Result<()> {
        let store = WorkspaceStore::load(Self::store())?;
        let workspace = store.get(name).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("workspace {name:?} does not exist"),
            )
        })?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        runtime.block_on(crate::runtime::domain::Domain::serve(workspace))
    }

    fn store() -> std::path::PathBuf {
        paths::hl_root().join("workspaces.conf")
    }
}

#[cfg(test)]
mod process_status_tests {
    use super::ProcessStatus;

    #[test]
    fn native_faults_do_not_wrap_into_success_like_exit_codes() {
        assert_eq!(ProcessStatus::from_engine(-1).0, 74);
        assert_eq!(ProcessStatus::from_engine(256).0, 74);
        assert_eq!(ProcessStatus::from_engine(0).0, 0);
        assert_eq!(ProcessStatus::from_engine(137).0, 137);
    }
}
