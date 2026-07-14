//! `hl app` — launch the installed hl-app GUI bundle (or a dev sibling binary).

use crate::report::run_status;
use std::process::Command;

/// Launch the installed GUI bundle (or a dev `hl-app` sibling binary).
pub(crate) fn cmd_app() -> i32 {
    if let Some(bundle) = crate::platform::app_bundle() {
        if bundle.exists() {
            // `open` detaches the GUI from this terminal.
            return run_status(Command::new("open").arg(&bundle));
        }
    }
    // Dev fallback: a hl-app binary next to us.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sib) = exe.parent().map(|d| d.join("hl-app")) {
            if sib.exists() {
                return run_status(&mut Command::new(sib));
            }
        }
    }
    eprintln!(
        "hl-app not found. Install it (drag hl.app to /Applications) or build with `make app`."
    );
    1
}
