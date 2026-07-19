//! Private workspace worker used by the signed application.

use crate::config::WorkspaceStore;
use crate::paths;
use hl_ws::Launcher;
use hl_ws_term::LocalShellLauncher;
use std::sync::atomic::{AtomicBool, Ordering};

mod terminal;

use terminal::TerminalSession;

static WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_signal: libc::c_int) {
    WINCH.store(true, Ordering::SeqCst);
}

/// Process-isolated operations used by terminal panes and workspace resource views.
pub struct Worker;

impl Worker {
    pub fn launch(name: &str, restore: bool, cwd: Option<&str>, slot: Option<&str>) -> ! {
        let store = WorkspaceStore::load(Self::store());
        let Some(workspace) = store.get(name).cloned() else {
            eprintln!("workspace {name:?} does not exist");
            std::process::exit(2);
        };
        let (columns, rows) = terminal::size().unwrap_or((80, 24));
        let launched =
            crate::runtime::execution::launch_ex(&workspace, columns, rows, restore, cwd, slot)
                .or_else(|error| {
                    eprintln!("[husklet] engine unavailable ({error}); using a local shell");
                    LocalShellLauncher::default().launch(&workspace, columns, rows)
                });
        let mut terminal = match launched {
            Ok(terminal) => terminal,
            Err(error) => {
                eprintln!("workspace launch failed: {error}");
                std::process::exit(1);
            }
        };
        std::process::exit(TerminalSession::run(&mut *terminal));
    }

    pub fn checkpoint(name: &str, slot: Option<&str>) -> i32 {
        let store = WorkspaceStore::load(Self::store());
        let Some(workspace) = store.get(name) else {
            eprintln!("workspace {name:?} does not exist");
            return 2;
        };
        let (pid_file, directory) = slot.map_or_else(
            || {
                (
                    workspace.checkpoint_pid_file(&paths::hl_root()),
                    workspace.checkpoint_dir(&paths::hl_root()),
                )
            },
            |slot| {
                (
                    workspace.checkpoint_slot_pid_file(&paths::hl_root(), slot),
                    workspace.checkpoint_slot_dir(&paths::hl_root(), slot),
                )
            },
        );
        let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        else {
            eprintln!("workspace {name:?} is not running");
            return 1;
        };
        let runtime = match hl_jit::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("checkpoint unavailable: {error}");
                return 1;
            }
        };
        match runtime.checkpoint(
            pid,
            &directory.to_string_lossy(),
            std::time::Duration::from_secs(30),
        ) {
            Ok(()) => {
                let _ = std::fs::remove_file(pid_file);
                0
            }
            Err(error) => {
                eprintln!("workspace checkpoint failed: {error}");
                1
            }
        }
    }

    pub fn daemon(name: &str) -> std::io::Result<std::path::PathBuf> {
        crate::runtime::resources::Daemon::new(name).ensure()
    }

    fn store() -> std::path::PathBuf {
        paths::hl_root().join("workspaces.conf")
    }
}
