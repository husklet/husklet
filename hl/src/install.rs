//! `hl install` / `hl uninstall` — set up or tear down the daemon service, docker context,
//! and (on macOS) Gatekeeper quarantine hints. All OS-specific work goes through `crate::platform`.

use crate::agent;
use crate::context;
use crate::paths;
use crate::platform;

/// Full install: state tree + daemon service + docker context, then a quarantine hint.
pub(crate) fn cmd_install() -> i32 {
    let unit = match agent::write_unit() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("write daemon service: {e}");
            return 1;
        }
    };
    println!("✓ wrote {}", unit.display());

    match agent::ensure() {
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
    println!("\nDone. Try:  hl ubuntu   (a shell in an ubuntu container, here)");
    0
}

pub(crate) fn cmd_uninstall(purge: bool) -> i32 {
    let _ = agent::remove();
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

/// On macOS, if the installed `.app` is Gatekeeper-quarantined, print the one-time fix. No-op
/// on platforms without an app bundle / Gatekeeper.
fn warn_quarantine() {
    if let Some(bundle) = platform::app_bundle() {
        if bundle.exists() && platform::is_quarantined(&bundle) {
            println!("\nThe app is quarantined by Gatekeeper. Clear it once with:");
            println!("    xattr -dr com.apple.quarantine {}", bundle.display());
        }
    }
}
