//! `ddcli install` / `ddcli uninstall` — set up or tear down the daemon agent, docker context, and Gatekeeper quarantine hints.

use crate::agent;
use crate::context;
use crate::doctor::ensure_agent;
use crate::paths;
use std::process::Command;

/// Full install: state tree + LaunchAgent + docker context, then a health hint.
pub(crate) fn cmd_install() -> i32 {
    if let Err(e) = agent::write_plist() {
        eprintln!("write LaunchAgent: {e}");
        return 1;
    }
    println!("✓ wrote {}", paths::agent_plist().display());

    match ensure_agent() {
        Ok(_) => println!("✓ loaded daemon agent ({})", agent::service_target()),
        Err(e) => eprintln!("! could not load agent: {e}"),
    }
    match context::create() {
        Ok(note) => println!("✓ {note}"),
        Err(e) => eprintln!("! docker context: {e}"),
    }
    let _ = context::use_context().map(|n| println!("✓ {n}"));

    println!("\nIf you don't use `docker context`, add this to your shell:");
    println!("    export DOCKER_HOST={}", paths::docker_host());
    warn_quarantine();
    println!(
        "\nDone. Try:  ddcli ubuntu   (a shell in an ubuntu container, here)  ·  ddcli doctor"
    );
    0
}

pub(crate) fn cmd_uninstall(purge: bool) -> i32 {
    let _ = agent::bootout();
    let _ = std::fs::remove_file(paths::agent_plist());
    println!("✓ removed daemon agent + plist");
    let _ = context::remove();
    println!("✓ removed docker context '{}'", context::NAME);
    if purge {
        let _ = std::fs::remove_dir_all(paths::hl_root());
        let _ = std::fs::remove_dir_all(paths::logs_dir());
        println!("✓ purged {} and logs", paths::hl_root().display());
    }
    0
}

pub(crate) fn is_quarantined(p: &std::path::Path) -> bool {
    Command::new("xattr")
        .arg("-p")
        .arg("com.apple.quarantine")
        .arg(p)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn warn_quarantine() {
    let bundle = std::path::Path::new(paths::APP_BUNDLE);
    if bundle.exists() && is_quarantined(bundle) {
        println!("\nThe app is quarantined by Gatekeeper. Clear it once with:");
        println!("    xattr -dr com.apple.quarantine {}", paths::APP_BUNDLE);
    }
}
