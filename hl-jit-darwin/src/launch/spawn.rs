//! The fork/exec entry: [`spawn`] / [`spawn_io`] / [`SpawnIo`] hand the encoded [`LaunchConfig`] wire
//! buffer to the C `hl_spawn` shim, which posix_spawns the arch-matching engine and returns its pid.

use super::LaunchConfig;
use crate::Guest;
use std::ffi::CString;
use std::os::fd::RawFd;
use std::os::raw::{c_char, c_int};

/// `flags` bit: the child leads a new process group (see `HL_SPAWN_SETPGID` in hl_api.h).
const HL_SPAWN_SETPGID: u32 = 0x1;
/// `flags` bit: the child acquires a controlling terminal (see `HL_SPAWN_TTY` in hl_api.h).
const HL_SPAWN_TTY: u32 = 0x2;

extern "C" {
    /// C spawn shim (os/darwin/ffi.c). `in_fd`/`out_fd`/`err_fd` become the child's fd 0/1/2 (-1 =
    /// inherit); `flags` is a bitwise-OR of `HL_JIT_SPAWN_*`. Returns the child pid, or -1 (errno set).
    /// The caller owns the passed fds and closes its own copies after this returns.
    fn hl_spawn(
        engine_path: *const c_char,
        config: *const u8,
        config_len: usize,
        in_fd: c_int,
        out_fd: c_int,
        err_fd: c_int,
        flags: u32,
    ) -> i32;
}

/// How to wire the launched child's stdio + placement. Every fd is a raw descriptor the CALLER owns —
/// the shim dup2's it onto the child's fd 0/1/2 and never closes the caller's copy. `-1` = inherit.
#[derive(Clone, Copy, Debug)]
pub struct SpawnIo {
    /// child fd 0 (`-1` = inherit the host's stdin)
    pub stdin: RawFd,
    /// child fd 1 (`-1` = inherit the host's stdout)
    pub stdout: RawFd,
    /// child fd 2 (`-1` = inherit the host's stderr)
    pub stderr: RawFd,
    /// place the child in its own process group (killpg/pause reach the whole container)
    pub setpgid: bool,
    /// give the child a controlling terminal (setsid + TIOCSCTTY); pass the pty SLAVE as stdin/out/err
    pub tty: bool,
}

impl Default for SpawnIo {
    /// Inherit the host stdio, no new process group, no controlling terminal.
    fn default() -> Self {
        SpawnIo { stdin: -1, stdout: -1, stderr: -1, setpgid: false, tty: false }
    }
}

/// Launch a container in the matching guest's engine, wiring the child's stdio + placement per `io`.
/// Returns the container pid, or an error if the engine for `guest` isn't built or the spawn failed.
pub fn spawn_io(guest: Guest, cfg: &LaunchConfig, io: SpawnIo) -> std::io::Result<u32> {
    let engine = guest
        .jit_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no engine built for guest"))?;
    let engine_c = CString::new(engine).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let wire = cfg.to_wire();
    let mut flags = 0u32;
    if io.setpgid {
        flags |= HL_SPAWN_SETPGID;
    }
    if io.tty {
        flags |= HL_SPAWN_TTY;
    }
    // SAFETY: `wire` is a live, correctly-sized buffer; `engine_c` is a valid NUL-terminated path; the
    // fds are the caller's own and stay open across this call (the shim only dup2's them in the child).
    let pid = unsafe {
        hl_spawn(
            engine_c.as_ptr(),
            wire.as_ptr(),
            wire.len(),
            io.stdin as c_int,
            io.stdout as c_int,
            io.stderr as c_int,
            flags,
        )
    };
    if pid <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(pid as u32)
}

/// Launch a container inheriting the host stdio (no new process group / terminal) — the convenience
/// path for the synchronous [`crate`] runner. Returns the container pid, or an error.
pub fn spawn(guest: Guest, cfg: &LaunchConfig) -> std::io::Result<u32> {
    spawn_io(guest, cfg, SpawnIo::default())
}
