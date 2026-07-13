//! `ddcli app` — launch the installed dd-app GUI bundle (or a dev sibling binary).

use crate::paths;
use crate::report::run_status;
use std::process::Command;

/// Launch the installed GUI bundle (or a dev `dd-app` sibling binary).
pub(crate) fn cmd_app() -> i32 {
    let bundle = std::path::Path::new(paths::APP_BUNDLE);
    if bundle.exists() {
        // `open` detaches the GUI from this terminal.
        return run_status(Command::new("open").arg(bundle));
    }
    // Dev fallback: a dd-app binary next to us.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sib) = exe.parent().map(|d| d.join("dd-app")) {
            if sib.exists() {
                return run_status(&mut Command::new(sib));
            }
        }
    }
    eprintln!(
        "dd-app not found. Install it (drag dd.app to /Applications) or build with `make app`."
    );
    1
}
