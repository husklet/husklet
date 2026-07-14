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
    /// Launch the dd-app GUI.
    App,
    /// Manage + launch terminal workspaces (a named image+arch you develop in).
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCmd,
    },
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
pub(crate) enum WorkspaceCmd {
    /// List configured workspaces.
    List,
    /// Create (or update) a workspace: a name + the image it runs + its architecture.
    Create {
        /// Workspace name (a stable handle you launch by).
        name: String,
        /// The image/distro the workspace runs, e.g. `ubuntu:24.04` or `alpine`.
        #[arg(long)]
        image: String,
        /// Target arch: `arm64` (default) or `amd64` (x86-64 via jit86). Omit when re-creating an existing
        /// workspace to preserve its current arch (a fresh workspace defaults to `arm64`); passing it
        /// always sets the arch explicitly.
        #[arg(long)]
        arch: Option<String>,
        /// Route this workspace's egress through a VPN/proxy (see docs/VPN.md). Accepts a bare SOCKS5
        /// `host:port` (e.g. `127.30.0.1:1080`) or a `<kind>:<endpoint>` spec
        /// (`socks5:host:port`, `http:host:port`, `wireguard:/path/wg.conf`). Omit for direct egress.
        #[arg(long)]
        vpn: Option<String>,
        /// Present a simulated CUDA device (docs/ideas/CUDA_ON_METAL.md): dd injects its NVML shim +
        /// the real `nvidia-smi` so the container reports an NVIDIA-looking GPU (presence only, not
        /// compute). Bare `--cuda` = the default device; `--cuda "Name|8.6|8192"` sets name|cc|VRAM-MB;
        /// `--cuda off` (or `""`/`none`) clears it. Omit to preserve any prior setting.
        #[arg(long, num_args = 0..=1, default_missing_value = "default")]
        cuda: Option<String>,
        /// Render this workspace's GUI apps on the Mac (docs/ideas/RENDERING_PLAN.md): dd bind-mounts the
        /// host `dd-display` Wayland socket into the guest and sets `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`, so
        /// a Linux GUI app (e.g. `weston-simple-shm`, SDL2) draws in a native window — no custom image.
        /// Bare `--gui` = on; `--gui off` clears it. Omit to preserve any prior setting.
        #[arg(long, num_args = 0..=1, default_missing_value = "on")]
        gui: Option<String>,
    },
    /// Remove a workspace (its persistent files are left on disk).
    Rm {
        name: String,
    },
    /// Launch a workspace as an interactive terminal in this window.
    Launch {
        name: String,
        /// Resume the workspace from its last checkpoint (whole process tree) instead of a fresh shell.
        #[arg(long)]
        restore: bool,
        /// Start the shell in this guest directory (used by the GUI's OSC-7 "new tab in same cwd").
        #[arg(long)]
        cwd: Option<String>,
        /// Per-pane checkpoint SLOT. A multi-tab/split window runs one engine per pane; each pane
        /// freezes/restores into its own `<storage>/checkpoint/<slot>` slot. Omit for the single
        /// shared slot (back-compat).
        #[arg(long)]
        slot: Option<String>,
    },
    /// Checkpoint a RUNNING workspace's whole process tree to disk (shells + background jobs + children),
    /// freeing its memory. Reopen it later with `workspace launch <name> --restore`.
    Checkpoint {
        name: String,
        /// Per-pane checkpoint SLOT (must match the slot the pane was launched with). Omit for the
        /// single shared slot (back-compat).
        #[arg(long)]
        slot: Option<String>,
    },
    /// Restore a checkpointed workspace's whole process tree (alias for `launch --restore`).
    Restore {
        name: String,
    },
    /// Ensure the workspace's isolated docker daemon is running; print its socket path.
    Daemon {
        name: String,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    // The parser is a contract with the GUI (which shells out to `ddcli workspace …`) and users, so lock
    // its shape down: required flags, the optional `--arch`, the trailing-arg passthrough, and the bare
    // `ddcli <image>` external-subcommand shorthand.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn workspace_create_minimal_has_none_optionals() {
        let cli = parse(&["ddcli", "workspace", "create", "foo", "--image", "alpine"]).unwrap();
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
        assert!(parse(&["ddcli", "workspace", "create", "foo"]).is_err());
        // A flag given with no value is also an error (no accidental empty-string image).
        assert!(parse(&["ddcli", "workspace", "create", "foo", "--image"]).is_err());
    }

    #[test]
    fn workspace_create_flag_arities() {
        // `--cuda` and `--gui` take an OPTIONAL value (num_args 0..=1): bare and valued both parse.
        let bare = parse(&["ddcli", "workspace", "create", "g", "--image", "x", "--cuda", "--gui"]).unwrap();
        let Cmd::Workspace { action: WorkspaceCmd::Create { cuda, gui, .. } } = bare.cmd else { panic!() };
        assert_eq!(cuda.as_deref(), Some("default"), "bare --cuda => default_missing_value");
        assert_eq!(gui.as_deref(), Some("on"), "bare --gui => default_missing_value");

        let valued = parse(&["ddcli", "workspace", "create", "g", "--image", "x",
            "--cuda", "My GPU|8.6|8192", "--gui", "off", "--vpn", "socks5:1.2.3.4:1080", "--arch", "amd64"]).unwrap();
        let Cmd::Workspace { action: WorkspaceCmd::Create { cuda, gui, vpn, arch, .. } } = valued.cmd else { panic!() };
        assert_eq!(cuda.as_deref(), Some("My GPU|8.6|8192"));
        assert_eq!(gui.as_deref(), Some("off"));
        assert_eq!(vpn.as_deref(), Some("socks5:1.2.3.4:1080"));
        assert_eq!(arch.as_deref(), Some("amd64"));
    }

    #[test]
    fn workspace_launch_slot_and_cwd() {
        let cli = parse(&["ddcli", "workspace", "launch", "w", "--restore", "--slot", "s1", "--cwd", "/work"]).unwrap();
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
        let cli = parse(&["ddcli", "run", "ubuntu", "--platform", "linux/amd64", "echo", "-n", "hi"]).unwrap();
        let Cmd::Run { args } = cli.cmd else { panic!("expected run") };
        assert_eq!(args, vec!["ubuntu", "--platform", "linux/amd64", "echo", "-n", "hi"]);
    }

    #[test]
    fn bare_image_is_external_subcommand_shorthand() {
        // `ddcli <image> [cmd…]` (no known subcommand) routes to the external subcommand, forwarding argv.
        let cli = parse(&["ddcli", "alpine", "sh", "-c", "id"]).unwrap();
        let Cmd::Image(args) = cli.cmd else { panic!("expected external-subcommand Image") };
        assert_eq!(args, vec!["alpine", "sh", "-c", "id"]);
    }

    #[test]
    fn uninstall_purge_flag() {
        let no = parse(&["ddcli", "uninstall"]).unwrap();
        assert!(matches!(no.cmd, Cmd::Uninstall { purge: false }));
        let yes = parse(&["ddcli", "uninstall", "--purge"]).unwrap();
        assert!(matches!(yes.cmd, Cmd::Uninstall { purge: true }));
    }

    #[test]
    fn no_subcommand_is_error() {
        // `ddcli` with no args must not panic; clap returns a (help) error.
        assert!(parse(&["ddcli"]).is_err());
    }
}
