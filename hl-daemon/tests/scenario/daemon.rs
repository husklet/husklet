use super::*;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

// ---- the mac bridge ------------------------------------------------------------------------------
// On a linux dev host, hl-daemon + its docker live mac-side: we run script FILES through `mac bash
// <file>` (env inline in the file, paths under a /Users shared dir). On macOS we run them directly.
// Script files (not `-lc` strings) sidestep all quote-escaping of embedded workloads/heredocs.
fn on_mac_host() -> bool {
    cfg!(target_os = "macos")
}
pub(super) fn shared_run_dir() -> PathBuf {
    // Must be visible to the mac side → under the repo (/Users/... shared mount), not /tmp.
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/hl-scen");
    std::fs::create_dir_all(&d).ok();
    d
}
/// Run a generated bash script file, optionally bridged to the mac side, under a linux `timeout`.
pub(super) fn run_script(file: &std::path::Path, bridged: bool, timeout_s: u64) -> std::process::Output {
    let mut c = Command::new("timeout");
    c.arg(timeout_s.to_string());
    if bridged && !on_mac_host() {
        c.arg("mac").arg("bash").arg(file);
    } else {
        c.arg("bash").arg(file);
    }
    c.output()
        .unwrap_or_else(|e| panic!("run_script {}: {e}", file.display()))
}
/// Spawn a long-lived script (the daemon), bridged on linux. Returns the child to kill on teardown.
fn spawn_script(file: &std::path::Path, bridged: bool) -> std::io::Result<std::process::Child> {
    if bridged && !on_mac_host() {
        Command::new("mac").arg("bash").arg(file).spawn()
    } else {
        Command::new("bash").arg(file).spawn()
    }
}
pub(super) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---- the daemon (Dd backend only) ----------------------------------------------------------------
pub struct Daemon {
    child: Option<std::process::Child>,
    pub(super) dir: PathBuf,
    log: PathBuf,
    host: String,
    pub(super) bridged: bool,
}

impl Daemon {
    /// Last `n` lines of the daemon log — where engine diagnostics (`unhandled syscall`, `[jit86]
    /// UNIMPL`, CRASHDBG rip dumps) actually print. Empty for the Real backend.
    pub fn log_tail(&self, n: usize) -> String {
        if self.log.as_os_str().is_empty() {
            return String::new();
        }
        std::fs::read_to_string(&self.log)
            .map(|s| {
                let lines: Vec<&str> = s.lines().collect();
                lines[lines.len().saturating_sub(n)..].join("\n")
            })
            .unwrap_or_default()
    }
}

impl Daemon {
    /// Real backend → no daemon to manage (host docker is already up). Dd backend → boot hl-daemon
    /// (bridged on linux) on a private socket/state under the shared run dir.
    pub fn boot(cfg: &Cfg) -> Result<Daemon, String> {
        let bridged = !on_mac_host();
        if cfg.backend == Backend::Real {
            // Real oracle = the host's Docker Desktop, reached through the SAME `mac` bridge but with the
            // DEFAULT docker context (no DOCKER_HOST). No daemon to manage; we just need a script dir.
            let dir = shared_run_dir().join(format!("real-{}", std::process::id()));
            std::fs::create_dir_all(&dir).ok();
            return Ok(Daemon {
                child: None,
                dir,
                log: PathBuf::new(),
                host: String::new(),
                bridged,
            });
        }
        let dir = shared_run_dir().join(format!("hl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let sock = dir.join("dd.sock");
        let log = dir.join("daemon.log");
        let _ = std::fs::remove_file(&sock);
        // Start a fresh daemon on a PRIVATE socket/state. NO global pkill — many daemons run in
        // parallel (one engine per worker), so we record THIS daemon's pid and kill only it on teardown.
        let boot_sh = dir.join("boot.sh");
        std::fs::write(
            &boot_sh,
            format!(
                "echo $$ > {dir}/daemon.pid\nexport HL_IMAGES={img}\nexport HL_DOCKER_SOCK={sock}\n\
             export HL_STATE={state}\nexport HL_VOLUMES={vol}\nexec {bin} > {log} 2>&1\n",
                dir = sh_quote(&dir.to_string_lossy()),
                img = sh_quote(&cfg.images.to_string_lossy()),
                sock = sh_quote(&sock.to_string_lossy()),
                state = sh_quote(&dir.join("state.json").to_string_lossy()),
                vol = sh_quote(&dir.join("vol").to_string_lossy()),
                bin = sh_quote(&cfg.daemon_bin.to_string_lossy()),
                log = sh_quote(&log.to_string_lossy()),
            ),
        )
        .map_err(|e| e.to_string())?;
        let child = spawn_script(&boot_sh, bridged).map_err(|e| format!("spawn daemon: {e}"))?;
        for _ in 0..160 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if !sock.exists() {
            let tail = std::fs::read_to_string(&log).unwrap_or_default();
            return Err(format!(
                "hl-daemon failed to start; log tail:\n{}",
                tail.lines().rev().take(15).collect::<Vec<_>>().join("\n")
            ));
        }
        Ok(Daemon {
            child: Some(child),
            dir,
            log: log.clone(),
            host: format!("unix://{}", sock.display()),
            bridged,
        })
    }
    /// The directory to drop generated op/ensure scripts into: this daemon's private dir, or the
    /// shared run dir when it has none (Real backend without a per-run dir).
    pub(super) fn run_dir(&self) -> PathBuf {
        if self.dir.as_os_str().is_empty() {
            shared_run_dir()
        } else {
            self.dir.clone()
        }
    }
    pub(super) fn docker_host(&self) -> Option<&str> {
        if self.host.is_empty() {
            None
        } else {
            Some(&self.host)
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
        }
        if self.bridged && !self.dir.as_os_str().is_empty() {
            // reap ONLY this worker's mac-side daemon (by recorded pid) — never a sibling's.
            let k = self.dir.join("kill.sh");
            let body = format!("p=$(cat {}/daemon.pid 2>/dev/null); [ -n \"$p\" ] && kill \"$p\" 2>/dev/null; true\n",
                sh_quote(&self.dir.to_string_lossy()));
            if std::fs::write(&k, body).is_ok() {
                let _ = run_script(&k, true, 15);
            }
        }
        if !self.dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
