//! `ddcli doctor` — health checks plus the agent/socket probes shared with other commands.

use crate::agent;
use crate::install::is_quarantined;
use crate::paths;
use crate::report::line;

/// Health check: socket reachable, agent loaded, context set, app present/unquarantined.
pub(crate) fn cmd_doctor() -> i32 {
    let mut ok = true;

    let agent_loaded = agent::is_loaded();
    line(
        agent_loaded,
        &format!("daemon agent loaded ({})", agent::service_target()),
    );
    ok &= agent_loaded;

    let sock = paths::socket();
    let reachable = ping_socket(&sock);
    line(
        reachable,
        &format!("daemon socket reachable ({})", sock.display()),
    );
    ok &= reachable;

    let ctx_dir = paths::home().join(".docker/contexts/meta");
    let ctx = ctx_dir.exists();
    line(ctx, "docker context present (~/.docker/contexts)");

    let bundle = std::path::Path::new(paths::APP_BUNDLE);
    if bundle.exists() {
        line(true, &format!("app installed ({})", paths::APP_BUNDLE));
        let quarantined = is_quarantined(bundle);
        line(!quarantined, "app not gatekeeper-quarantined");
        if quarantined {
            println!(
                "    fix: xattr -dr com.apple.quarantine {}",
                paths::APP_BUNDLE
            );
        }
    } else {
        line(
            false,
            &format!("app not installed at {}", paths::APP_BUNDLE),
        );
        println!("    install: build with `make dmg`, then drag dd.app to /Applications");
    }

    if !ok {
        println!("\nSome checks failed. `ddcli install` sets up the agent + context.");
        println!(
            "If the GUI renders oddly, try:  GSK_RENDERER=cairo open {}",
            paths::APP_BUNDLE
        );
    }
    if ok {
        0
    } else {
        1
    }
}

/// Write the plist (if missing) and bootstrap the agent.
pub(crate) fn ensure_agent() -> Result<(), String> {
    if !paths::agent_plist().exists() {
        agent::write_plist().map_err(|e| e.to_string())?;
    }
    agent::bootstrap().map_err(|e| e.to_string())
}

/// Synchronously connect to the socket to confirm the daemon answers `_ping`.
pub(crate) fn ping_socket(sock: &std::path::Path) -> bool {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return false,
    };
    rt.block_on(async {
        let c = dd_client::Client::new(sock);
        c.ping().await.is_ok()
    })
}
