//! `hl daemon …` — run/start/stop/restart/status/logs for the background daemon.

use crate::agent;
use crate::paths;
use crate::report::{report, run_status};
use clap::Subcommand;
use std::process::Command;

/// `hl daemon <action>` — run/start/stop/restart/status/logs for the background daemon. The clap
/// arg type lives next to its handler; the binary's top-level `Cmd` references it as
/// `hl::daemon::DaemonCmd`.
#[derive(Subcommand)]
pub enum DaemonCmd {
    /// Run the daemon in the foreground (what the LaunchAgent execs).
    Run,
    /// Load + start the daemon agent.
    Start,
    /// Stop + unload the daemon agent.
    Stop,
    /// Restart the daemon agent.
    Restart,
    /// Show launchd status for the agent.
    Status,
    /// Tail the daemon logs.
    Logs,
}

pub fn cmd_daemon(action: DaemonCmd) -> i32 {
    match action {
        DaemonCmd::Run => daemon_run(),
        DaemonCmd::Start => report("daemon start", agent::ensure()),
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

/// Exec the hl-daemon binary in the foreground with the canonical env.
fn daemon_run() -> i32 {
    use std::os::unix::process::CommandExt;
    let _ = std::fs::create_dir_all(paths::run_dir());
    let _ = std::fs::create_dir_all(paths::images_dir());
    let bin = paths::daemon_bin();
    if !bin.exists() {
        eprintln!("hl-daemon binary not found at {}", bin.display());
        return 1;
    }
    let mut cmd = Command::new(&bin);
    cmd.env("HL_DOCKER_SOCK", paths::socket())
        .env("HL_IMAGES", paths::images_dir());
    if let Some(dir) = bin.parent() {
        cmd.env("HL_JIT_DIR", dir); // find hljit-* next to the daemon (bundle Resources)
    }
    let err = cmd.exec(); // only returns on failure
    eprintln!("exec {} failed: {err}", bin.display());
    1
}
