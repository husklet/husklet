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
    pub fn launch(name: &str, cwd: Option<&str>, slot: Option<&str>) -> ! {
        let store = WorkspaceStore::load(Self::store());
        let Some(workspace) = store.get(name).cloned() else {
            eprintln!("workspace {name:?} does not exist");
            std::process::exit(2);
        };
        let (columns, rows) = terminal::size().unwrap_or((80, 24));
        let launched = crate::runtime::execution::launch(&workspace, columns, rows, cwd, slot)
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

    pub fn daemon(name: &str) -> std::io::Result<std::path::PathBuf> {
        crate::runtime::resources::Daemon::new(name).ensure()
    }

    fn store() -> std::path::PathBuf {
        paths::hl_root().join("workspaces.conf")
    }
}
