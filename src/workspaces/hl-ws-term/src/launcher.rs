//! `LocalShellLauncher` — the host-shell implementation of `hl-ws`'s [`Launcher`] seam.
//!
//! Runs a plain host shell (ignoring the image) so the whole configure→launch→terminal flow is
//! exercisable on any host and in tests. The real engine launcher (`HlJitLauncher`, macOS) lives in `hl`
//! and enters the image's container; both return the shared [`hl_ws::PtyBackend`] handle.

use crate::pty::local::LocalPty;
use hl_ws::{Launcher, PtyBackend, Workspace};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// A launcher that runs a plain host shell (ignoring the image) — used on hosts without the container
/// engine and in tests.
pub struct LocalShellLauncher {
    pub shell: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

impl Default for LocalShellLauncher {
    fn default() -> Self {
        let sh = if Path::new("/bin/bash").exists() {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        LocalShellLauncher {
            shell: vec![sh.to_string()],
            environment: BTreeMap::new(),
        }
    }
}

impl Launcher for LocalShellLauncher {
    fn launch(&self, ws: &Workspace, cols: u16, rows: u16) -> io::Result<Box<dyn PtyBackend>> {
        let argv: Vec<&str> = self.shell.iter().map(|s| s.as_str()).collect();
        let mut environment = self.environment.clone();
        environment.insert("TERM".into(), "xterm-256color".into());
        environment.insert("HL_WORKSPACE".into(), ws.name.clone());
        let pty = LocalPty::spawn(&argv, cols, rows, &environment)?;
        Ok(Box::new(pty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vt;
    use hl_ws::Arch;
    use std::time::{Duration, Instant};

    #[test]
    fn launch_workspace_runs_a_terminal() {
        // The whole configure→launch→terminal flow, headless: a workspace launches a shell we can drive.
        let ws = Workspace::new("demo", "ubuntu:24.04", Arch::Arm64);
        let launcher = LocalShellLauncher::default();
        let mut pty = launcher.launch(&ws, 40, 10).unwrap();
        pty.write(b"echo hello-$HL_WORKSPACE; exit\n").unwrap();

        let mut vt = Vt::new(40, 10);
        let fd = pty.master_fd().unwrap();
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        loop {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let pr = unsafe { libc::poll(&mut pfd, 1, 20) };
            if pr > 0 && pfd.revents & libc::POLLIN != 0 {
                let n = pty.read(&mut buf).unwrap_or(0);
                if n > 0 {
                    vt.advance_bytes(&buf[..n]);
                    continue;
                }
            }
            if exited || Instant::now() > deadline {
                break;
            }
            if pty.try_wait().is_some() {
                exited = true;
            }
        }
        let screen: String = (0..10)
            .map(|r| vt.grid().row_text(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("hello-demo"),
            "workspace shell should have run the command; got:\n{screen}"
        );
    }
}
