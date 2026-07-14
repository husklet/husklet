//! The launch seam: turn a [`Workspace`] into a live terminal ([`crate::terminal::PtyBackend`]).
//!
//! Only the TRAIT lives here (std-only). The concrete launchers implement it in the crates that own a
//! terminal: `LocalShellLauncher` in `hl-ws-term` (host shell, tests) and the engine `DdJitLauncher` in
//! `hl`. `hl-ws` names no concrete terminal — the returned handle is the abstract [`PtyBackend`] trait.

use crate::model::Workspace;
use crate::terminal::PtyBackend;
use std::io;

/// Turn a configured [`Workspace`] into a live terminal.
pub trait Launcher {
    fn launch(&self, ws: &Workspace, cols: u16, rows: u16) -> io::Result<Box<dyn PtyBackend>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Arch;

    /// A std-only stub proving the `Launcher`/`PtyBackend` seam is usable without any terminal crate.
    struct NullPty;
    impl PtyBackend for NullPty {
        fn write(&mut self, _b: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn read(&mut self, _b: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn resize(&mut self, _c: u16, _r: u16) {}
        fn master_fd(&self) -> Option<std::os::unix::io::RawFd> {
            None
        }
        fn try_wait(&mut self) -> Option<i32> {
            Some(0)
        }
    }

    struct StubLauncher;
    impl Launcher for StubLauncher {
        fn launch(&self, _ws: &Workspace, _c: u16, _r: u16) -> io::Result<Box<dyn PtyBackend>> {
            Ok(Box::new(NullPty))
        }
    }

    #[test]
    fn launcher_trait_is_object_safe_and_usable() {
        let ws = Workspace::new("t", "img", Arch::Arm64);
        let l: Box<dyn Launcher> = Box::new(StubLauncher);
        let mut pty = l.launch(&ws, 80, 24).unwrap();
        assert_eq!(pty.try_wait(), Some(0));
    }
}
