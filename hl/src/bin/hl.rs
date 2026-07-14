//! `hl` — the user-facing command for the dd VM-less container runtime.
//!
//! This binary is the PARSING LAYER only: it owns the clap `Cli`/`Cmd` (the top-level command grammar)
//! and `fn main`, then dispatches into `hl::*` library fns which hold all the command logic. The
//! per-command sub-grammars (`WorkspaceCmd`/`DaemonCmd`/`ContextCmd`) live next to their handlers in the
//! library (`hl::workspace`/`hl::daemon`/`hl::context`) and are referenced here.
//!
//! Run containers with easy-access defaults (the current directory mounted + the working dir, host
//! networking, an interactive shell), and manage the per-user daemon — all without root.
//!
//!   hl ubuntu                       # drop into a shell in an ubuntu container, here in this dir
//!   hl run alpine echo hi           # run a one-off command
//!   hl run ubuntu --platform linux/amd64   # force amd64 (runs via the x86-64 JIT)
//!   hl install                      # set up the daemon agent + docker context

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hl", version, about = "hl — VM-less containers on macOS")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a container: current dir mounted + working dir, host networking, interactive shell.
    ///
    /// Usage: hl run [--platform P] [--isolated] [--keep] <image> [command…]
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Launch the hl-app GUI.
    App,
    /// Manage + launch terminal workspaces (a named image+arch you develop in).
    Workspace {
        #[command(subcommand)]
        action: hl::workspace::WorkspaceCmd,
    },
    /// Run or control the background daemon.
    Daemon {
        #[command(subcommand)]
        action: hl::daemon::DaemonCmd,
    },
    /// Install the daemon agent + docker context (no root).
    Install,
    /// Remove the daemon agent + docker context.
    Uninstall {
        /// Also delete ~/.hl state (images, volumes, state.json) and logs.
        #[arg(long)]
        purge: bool,
    },
    /// Manage just the docker context.
    Context {
        #[command(subcommand)]
        action: hl::context::ContextCmd,
    },
    /// `hl <image> [command…]` — shorthand for `hl run <image> …`.
    #[command(external_subcommand)]
    Image(Vec<String>),
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Run { args } => hl::run::cmd_run(args),
        Cmd::Image(args) => hl::run::cmd_run(args),
        Cmd::App => hl::app::cmd_app(),
        Cmd::Workspace { action } => {
            hl::workspace::run(action);
            0
        }
        Cmd::Daemon { action } => hl::daemon::cmd_daemon(action),
        Cmd::Install => hl::install::cmd_install(),
        Cmd::Uninstall { purge } => hl::install::cmd_uninstall(purge),
        Cmd::Context { action } => hl::context::cmd_context(action),
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl::workspace::WorkspaceCmd;

    // The parser is a contract with the GUI (which shells out to `hl workspace …`) and users, so lock
    // its shape down: required flags, the optional `--arch`, the trailing-arg passthrough, and the bare
    // `hl <image>` external-subcommand shorthand.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn workspace_create_minimal_has_none_optionals() {
        let cli = parse(&["hl", "workspace", "create", "foo", "--image", "alpine"]).unwrap();
        let Cmd::Workspace { action: WorkspaceCmd::Create { name, image, arch, vpn, cuda, gui } } = cli.cmd else {
            panic!("expected workspace create");
        };
        assert_eq!(name, "foo");
        assert_eq!(image, "alpine");
        // `--arch` must be OPTIONAL (no clap default) so re-create can preserve the prior arch.
        assert_eq!(arch, None, "--arch must default to None, not \"arm64\"");
        assert_eq!((vpn, cuda, gui), (None, None, None));
    }

    #[test]
    fn workspace_create_requires_image() {
        // `--image` is a required flag: omitting it is a parse error, not a silent empty image.
        assert!(parse(&["hl", "workspace", "create", "foo"]).is_err());
        // A flag given with no value is also an error (no accidental empty-string image).
        assert!(parse(&["hl", "workspace", "create", "foo", "--image"]).is_err());
    }

    #[test]
    fn workspace_create_flag_arities() {
        // `--cuda` and `--gui` take an OPTIONAL value (num_args 0..=1): bare and valued both parse.
        let bare = parse(&["hl", "workspace", "create", "g", "--image", "x", "--cuda", "--gui"]).unwrap();
        let Cmd::Workspace { action: WorkspaceCmd::Create { cuda, gui, .. } } = bare.cmd else { panic!() };
        assert_eq!(cuda.as_deref(), Some("default"), "bare --cuda => default_missing_value");
        assert_eq!(gui.as_deref(), Some("on"), "bare --gui => default_missing_value");

        let valued = parse(&["hl", "workspace", "create", "g", "--image", "x",
            "--cuda", "My GPU|8.6|8192", "--gui", "off", "--vpn", "socks5:1.2.3.4:1080", "--arch", "amd64"]).unwrap();
        let Cmd::Workspace { action: WorkspaceCmd::Create { cuda, gui, vpn, arch, .. } } = valued.cmd else { panic!() };
        assert_eq!(cuda.as_deref(), Some("My GPU|8.6|8192"));
        assert_eq!(gui.as_deref(), Some("off"));
        assert_eq!(vpn.as_deref(), Some("socks5:1.2.3.4:1080"));
        assert_eq!(arch.as_deref(), Some("amd64"));
    }

    #[test]
    fn workspace_launch_slot_and_cwd() {
        let cli = parse(&["hl", "workspace", "launch", "w", "--restore", "--slot", "s1", "--cwd", "/work"]).unwrap();
        let Cmd::Workspace { action: WorkspaceCmd::Launch { name, restore, cwd, slot } } = cli.cmd else { panic!() };
        assert_eq!(name, "w");
        assert!(restore);
        assert_eq!(cwd.as_deref(), Some("/work"));
        assert_eq!(slot.as_deref(), Some("s1"));
    }

    #[test]
    fn run_passes_through_trailing_args_and_hyphens() {
        // trailing_var_arg + allow_hyphen_values: everything after `run` is captured verbatim, including
        // `--platform` and the guest command's own flags — clap must NOT try to interpret them.
        let cli = parse(&["hl", "run", "ubuntu", "--platform", "linux/amd64", "echo", "-n", "hi"]).unwrap();
        let Cmd::Run { args } = cli.cmd else { panic!("expected run") };
        assert_eq!(args, vec!["ubuntu", "--platform", "linux/amd64", "echo", "-n", "hi"]);
    }

    #[test]
    fn bare_image_is_external_subcommand_shorthand() {
        // `hl <image> [cmd…]` (no known subcommand) routes to the external subcommand, forwarding argv.
        let cli = parse(&["hl", "alpine", "sh", "-c", "id"]).unwrap();
        let Cmd::Image(args) = cli.cmd else { panic!("expected external-subcommand Image") };
        assert_eq!(args, vec!["alpine", "sh", "-c", "id"]);
    }

    #[test]
    fn uninstall_purge_flag() {
        let no = parse(&["hl", "uninstall"]).unwrap();
        assert!(matches!(no.cmd, Cmd::Uninstall { purge: false }));
        let yes = parse(&["hl", "uninstall", "--purge"]).unwrap();
        assert!(matches!(yes.cmd, Cmd::Uninstall { purge: true }));
    }

    #[test]
    fn no_subcommand_is_error() {
        // `hl` with no args must not panic; clap returns a (help) error.
        assert!(parse(&["hl"]).is_err());
    }
}
