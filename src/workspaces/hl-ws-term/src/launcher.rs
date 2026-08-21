//! `LocalShellLauncher` — the host-shell implementation of `hl-ws`'s [`Launcher`] seam.
//!
//! Runs a plain host shell (ignoring the image) so the whole configure→launch→terminal flow is
//! exercisable on any host and in tests. The real engine launcher (`HlJitLauncher`, macOS) lives in `hl`
//! and enters the image's container; both return the shared [`hl_ws::PtyBackend`] handle.

#[cfg(unix)]
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
    #[cfg(unix)]
    fn launch(&self, ws: &Workspace, cols: u16, rows: u16) -> io::Result<Box<dyn PtyBackend>> {
        let argv: Vec<&str> = self.shell.iter().map(std::string::String::as_str).collect();
        let mut environment = self.environment.clone();
        environment.insert("TERM".into(), "xterm-256color".into());
        environment.insert("HL_WORKSPACE".into(), ws.name.clone());
        let pty = LocalPty::spawn(&argv, cols, rows, &environment)?;
        Ok(Box::new(pty))
    }

    /// The seam stays on every host; what is missing is the backend behind it, and the error says which
    /// one. `LocalPty` is the POSIX-98 pty (`posix_openpt`/`grantpt`/`unlockpt`/`ptsname`) plus
    /// `fork`/`execvp`, and Windows has none of those calls — its pseudoconsole is `CreatePseudoConsole`
    /// with a `STARTUPINFOEX` attribute list, a different mechanism that nothing in this crate implements
    /// yet. Refusing here, named, is what keeps a Windows `hl-ws-term` from silently having no `Launcher`
    /// at all: the type resolves, `Default` resolves, and the one call that cannot work says why.
    #[cfg(not(unix))]
    fn launch(&self, ws: &Workspace, _cols: u16, _rows: u16) -> io::Result<Box<dyn PtyBackend>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "workspace {:?}: LocalShellLauncher has no pty backend on this host. Its only backend is \
                 hl_ws_term::pty::local::LocalPty, which is posix_openpt/grantpt/unlockpt/ptsname plus \
                 fork/execvp and is compiled under cfg(unix); the Windows equivalent, a ConPTY backend \
                 built on CreatePseudoConsole, is not implemented",
                ws.name
            ),
        ))
    }
}

/// The seam itself is the thing that must survive the platform, and this says so in a form the
/// compiler checks on every target rather than only where a test runs. The cheap way to make a
/// Windows `hl-ws-term` compile is to put `#[cfg(unix)]` on the `impl Launcher` block above and be
/// done -- the crate builds, the mingw check goes green, and `LocalShellLauncher` quietly stops
/// being a `Launcher` at all on that host. This line is what refuses that: only the BACKEND is
/// allowed to be absent off Unix, never the implementation of the seam.
const _: fn() = || {
    fn only_the_backend_is_platform_specific<T: Launcher>() {}
    only_the_backend_is_platform_specific::<LocalShellLauncher>();
};

// This module's one test spawns a real host shell on a real pty and reads its output back, so its
// subject IS the Unix mechanism: there is nothing for it to assert where `launch` refuses.
#[cfg(all(test, unix))]
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
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        loop {
            let n = pty.read(&mut buf).unwrap_or(0);
            if n > 0 {
                vt.advance_bytes(&buf[..n]);
                continue;
            }
            if exited || Instant::now() > deadline {
                break;
            }
            if pty.try_wait().is_some() {
                exited = true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let screen: String = (0..10).map(|r| vt.grid().row_text(r)).collect::<Vec<_>>().join("\n");
        assert!(
            screen.contains("hello-demo"),
            "workspace shell should have run the command; got:\n{screen}"
        );
    }
}
