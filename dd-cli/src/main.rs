//! `ddcli` — the user-facing command for the dd VM-less container runtime.
//!
//! Run containers with easy-access defaults (the current directory mounted + the working dir, host
//! networking, an interactive shell), and manage the per-user daemon — all without root.
//!
//!   ddcli ubuntu                       # drop into a shell in an ubuntu container, here in this dir
//!   ddcli run alpine echo hi           # run a one-off command
//!   ddcli run ubuntu --platform linux/amd64   # force amd64 (runs via the x86-64 JIT)
//!   ddcli mac                          # a macOS container (experimental)
//!   ddcli install                      # set up the daemon agent + docker context
//!   ddcli doctor                       # check everything is healthy

mod agent;
mod app;
mod cli;
mod context;
mod daemon;
mod doctor;
mod install;
mod paths;
mod report;
mod run;

use crate::app::cmd_app;
use crate::cli::{Cli, Cmd};
use crate::context::cmd_context;
use crate::daemon::cmd_daemon;
use crate::doctor::cmd_doctor;
use crate::install::{cmd_install, cmd_uninstall};
use crate::run::cmd_run;
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Run { args } => cmd_run(args),
        Cmd::Mac { args } => run::mac(args),
        Cmd::Image(args) => cmd_run(args),
        Cmd::App => cmd_app(),
        Cmd::Daemon { action } => cmd_daemon(action),
        Cmd::Install => cmd_install(),
        Cmd::Uninstall { purge } => cmd_uninstall(purge),
        Cmd::Context { action } => cmd_context(action),
        Cmd::Doctor => cmd_doctor(),
    };
    std::process::exit(code);
}
