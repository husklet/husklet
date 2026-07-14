//! `hl` — the user-facing command for the dd VM-less container runtime.
//!
//! Run containers with easy-access defaults (the current directory mounted + the working dir, host
//! networking, an interactive shell), and manage the per-user daemon — all without root.
//!
//!   hl ubuntu                       # drop into a shell in an ubuntu container, here in this dir
//!   hl run alpine echo hi           # run a one-off command
//!   hl run ubuntu --platform linux/amd64   # force amd64 (runs via the x86-64 JIT)
//!   hl install                      # set up the daemon agent + docker context

mod agent;
mod app;
mod cli;
mod context;
mod daemon;
mod install;
mod paths;
mod platform;
mod hl_launcher;
mod report;
mod run;
mod workspace;
mod wsdaemon;

use crate::app::cmd_app;
use crate::cli::{Cli, Cmd};
use crate::context::cmd_context;
use crate::daemon::cmd_daemon;
use crate::install::{cmd_install, cmd_uninstall};
use crate::run::cmd_run;
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Run { args } => cmd_run(args),
        Cmd::Image(args) => cmd_run(args),
        Cmd::App => cmd_app(),
        Cmd::Workspace { action } => {
            workspace::run(action);
            0
        }
        Cmd::Daemon { action } => cmd_daemon(action),
        Cmd::Install => cmd_install(),
        Cmd::Uninstall { purge } => cmd_uninstall(purge),
        Cmd::Context { action } => cmd_context(action),
    };
    std::process::exit(code);
}
