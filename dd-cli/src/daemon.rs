//! `ddcli daemon …` — run/start/stop/restart/status/logs for the background daemon.

use crate::agent;
use crate::cli::DaemonCmd;
use crate::doctor::ensure_agent;
use crate::paths;
use crate::report::{report, run_status};
use std::process::Command;

pub(crate) fn cmd_daemon(action: DaemonCmd) -> i32 {
    match action {
        DaemonCmd::Run => daemon_run(),
        DaemonCmd::Start => report("daemon start", ensure_agent()),
        DaemonCmd::Stop => report("daemon stop", agent::bootout()),
        DaemonCmd::Restart => report("daemon restart", agent::kickstart()),
        DaemonCmd::Status => {
            let _ = agent::print_status();
            0
        }
        DaemonCmd::Logs => {
            let log = paths::logs_dir().join("daemon.err.log");
            run_status(Command::new("tail").args(["-n", "200", "-f"]).arg(log))
        }
    }
}

/// Exec the dd-daemon binary in the foreground with the canonical env.
fn daemon_run() -> i32 {
    use std::os::unix::process::CommandExt;
    let _ = std::fs::create_dir_all(paths::run_dir());
    let _ = std::fs::create_dir_all(paths::images_dir());
    let bin = paths::daemon_bin();
    if !bin.exists() {
        eprintln!("dd-daemon binary not found at {}", bin.display());
        return 1;
    }
    let mut cmd = Command::new(&bin);
    cmd.env("DDOCKERD_SOCK", paths::socket())
        .env("DD_IMAGES", paths::images_dir());
    if let Some(dir) = bin.parent() {
        cmd.env("DDJIT_DIR", dir); // find ddjit-* next to the daemon (bundle Resources)
    }
    let err = cmd.exec(); // only returns on failure
    eprintln!("exec {} failed: {err}", bin.display());
    1
}
