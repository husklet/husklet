//! `clap` command-line definitions for the `ddcli` binary.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ddcli", version, about = "ddcli — VM-less containers on macOS")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Run a container: current dir mounted + working dir, host networking, interactive shell.
    ///
    /// Usage: ddcli run [--platform P] [--isolated] [--keep] <image> [command…]
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start a macOS container (experimental — the host macOS in a darwin jail).
    Mac {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Launch the dd-app GUI.
    App,
    /// Run or control the background daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonCmd,
    },
    /// Install the daemon agent + docker context (no root).
    Install,
    /// Remove the daemon agent + docker context.
    Uninstall {
        /// Also delete ~/.dd state (images, volumes, state.json) and logs.
        #[arg(long)]
        purge: bool,
    },
    /// Manage just the docker context.
    Context {
        #[command(subcommand)]
        action: ContextCmd,
    },
    /// Diagnose the install (socket, agent, context, app quarantine).
    Doctor,
    /// `ddcli <image> [command…]` — shorthand for `ddcli run <image> …`.
    #[command(external_subcommand)]
    Image(Vec<String>),
}

#[derive(Subcommand)]
pub(crate) enum DaemonCmd {
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

#[derive(Subcommand)]
pub(crate) enum ContextCmd {
    /// Create/refresh the `dd` docker context.
    Create,
    /// Remove the `dd` docker context.
    Rm,
    /// `docker context use dd`.
    Use,
    /// Print the context endpoint.
    Show,
}
