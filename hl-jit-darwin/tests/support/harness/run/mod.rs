//! The per-case test RUNNER. `run()` is the driver: provision a guest, jail-copy it into the rootfs,
//! assemble the spawn config (`config`), drive the engine under `timeout`, drain guest stdout durably,
//! and evaluate the case's checks (`eval`). Shell/output helpers live in `util`.

use std::process::Command;

use super::*;

mod config;
mod eval;
mod util;

pub(crate) use config::{build_cfg, guest_argv};
use eval::eval;
use util::{shq, strip_noise};

/// Run one case on one engine and evaluate its checks.
pub fn run(ctx: &Ctx, c: &Case, e: Engine) -> Status {
    if !c.engines.contains(&e) {
        return Status::Skip("n/a for engine".into());
    }
    if !e.available() {
        return Status::Skip(format!("{} JIT not built", e.label()));
    }
    let guest = match provision(ctx, c, e) {
        Ok(Some(g)) => g,
        Ok(None) => return Status::Skip(format!("no {} guest", e.label())),
        Err(err) => return Status::Fail(err),
    };
    // scratch/distroless guard: synthesize an otherwise-EMPTY rootfs (just a `/tmp` landing dir for
    // the jailed guest copy below) — the FROM-scratch condition, with no shell/interpreter/libc on disk.
    // Self-contained (built under the cache tree, no poc image needed), so the loader/exec path is proven
    // to resolve + exec a static binary that is the sole executable in its rootfs.
    let rootfs = if c.scratch {
        let d = ctx.cache.join("scratchfs");
        if std::fs::create_dir_all(d.join("tmp")).is_err() {
            return Status::Skip("scratchfs create failed".into());
        }
        Some(d.to_string_lossy().into_owned())
    } else {
        c.rootfs.and_then(|r| ctx.rootfs_path(r, e))
    };
    if c.rootfs.is_some() && rootfs.is_none() {
        return Status::Skip(format!("no {} rootfs", e.label()));
    }

    // A COMPILED guest + a rootfs on a Linux engine: the engine resolves argv[0] INSIDE the jail
    // (xresolve_overlay at startup), so a host path outside the rootfs can never load. Copy the built
    // guest into the image's /tmp under a unique name and run it by its in-guest path (removed after
    // the run; the fixture rootfs' /tmp is already scratch for the sh-based cases).
    let mut jail_copy: Option<(String, String)> = None; // (host file to clean up, in-guest argv[0])
    if let Some(rfs) = &rootfs {
        if !matches!(c.bin, Bin::InRootfs) {
            let leaf = format!(
                "ddguest_{}_{}_{}",
                c.name.replace('/', "_"),
                e.arch(),
                std::process::id()
            );
            let host = format!("{rfs}/tmp/{leaf}");
            match std::fs::copy(&guest, &host) {
                Ok(_) => {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755));
                    jail_copy = Some((host, format!("/tmp/{leaf}")));
                }
                Err(err) => return Status::Fail(format!("copy guest into rootfs: {err}")),
            }
        }
    }

    let rootfs_str = rootfs.unwrap_or_default();
    let mut cfg = build_cfg(c, e, &rootfs_str);
    let argv0 = jail_copy
        .as_ref()
        .map(|(_, g)| g.clone())
        .unwrap_or_else(|| guest.clone());
    cfg.argv = guest_argv(c, argv0);
    let (prog, args) = match cfg.command(e.jit()) {
        Some(x) => x,
        None => return Status::Skip("no command".into()),
    };

    // ── Reliable guest-stdout capture across the `mac` bridge ─────────────────────────────────────
    // On a Linux dev host the engine runs mac-side and its stdout is streamed back to this runner by
    // the OrbStack `mac` bridge. Under host load that bridge occasionally DROPS a guest's FINAL
    // buffered stdout write at teardown while STILL propagating the exit code — so an otherwise-correct
    // result (rc=0, right value or empty, never a *wrong* value) is truncated to empty and the case
    // spuriously fails. Seen on epoll_oneshot / pidfd / posixtimer / threadrss. `.output`
    // already drains the bridge's pipe to EOF, but the bytes were lost UPSTREAM in the bridge, so no
    // reader-side drain can recover them. Fix: redirect the guest's stdout into a file on the shared
    // repo tree (the SAME absolute path is visible to both the mac-side engine and this Linux runner,
    // Golden Rule 4) and read it back AFTER the process exits. A file write is durable — the final line
    // survives any bridge-teardown race. Proven: a minimal `mac` write dropped 2/800 through the pipe
    // under a mac-side CPU flood; the file redirect dropped 0/800 under a heavier flood. stderr stays
    // on the pipe (diagnostics only, never asserted) and the exit code is unchanged (still the guest's,
    // propagated by the bridge). On a real Mac there is no bridge and no race — the same file capture
    // is equally correct — so the path is unified (no per-guest fflush/usleep workaround needed).
    //
    let drain_file = ctx.cache.join("stdout").join(format!(
        "{}_{}_{}.out",
        c.name.replace('/', "_"),
        e.arch(),
        std::process::id()
    ));
    let mut args = args;
    let drained = std::fs::create_dir_all(drain_file.parent().unwrap()).is_ok();
    if drained {
        let _ = std::fs::remove_file(&drain_file);
        // The launch script is the last arg (`… bash -lc <script>`); appending a stdout redirect binds
        // it to the trailing `exec … argv` command, so the guest inherits fd 1 = this file. fd 1 stays
        // a NON-tty (isatty(1)==0 for both a pipe and a regular file), so guest behaviour is unchanged.
        if let Some(script) = args.last_mut() {
            *script = format!("{} > {}", script, shq(&drain_file.to_string_lossy()));
        }
    }

    // Wrap in `timeout` so a hung/looping guest can't block the matrix (the x86 JIT can mistranslate
    // into an infinite loop). 124 = timed out.
    let out = Command::new("timeout")
        .arg("25")
        .arg(&prog)
        .args(&args)
        .output();
    if let Some((host, _)) = &jail_copy {
        let _ = std::fs::remove_file(host);
    }
    let out = match out {
        Ok(o) => o,
        Err(err) => {
            let _ = std::fs::remove_file(&drain_file);
            return Status::Fail(format!("spawn: {err}"));
        }
    };
    // Recover the guest's stdout from the drained file (durable; immune to the bridge-teardown drop);
    // fall back to the bridge pipe only if the redirect could not be set up. Then remove the file.
    let stdout_bytes: Vec<u8> = if drained {
        let b = std::fs::read(&drain_file).unwrap_or_default();
        let _ = std::fs::remove_file(&drain_file);
        b
    } else {
        out.stdout.clone()
    };
    // a known failure on this engine is reported xfail, not a regression
    let fail = |msg: String| {
        if c.xfail.contains(&e) {
            Status::Xfail(msg)
        } else {
            Status::Fail(msg)
        }
    };
    if out.status.code() == Some(124) {
        return fail(format!("timeout (>25s) [{}]", e.label()));
    }
    if std::env::var("DD_DEBUG").is_ok() {
        eprintln!(
            "\n[dbg] {} {:?}\n[dbg] out={:?}\n[dbg] err={:?}\n[dbg] code={:?}",
            prog,
            args,
            String::from_utf8_lossy(&stdout_bytes),
            String::from_utf8_lossy(&out.stderr),
            out.status.code()
        );
    }

    let stdout = strip_noise(&stdout_bytes);
    let code = out.status.code().unwrap_or(-1);
    for chk in &c.checks {
        if let Err(msg) = eval(chk, &stdout, code, &guest, &c.args, e) {
            if std::env::var("CRASHDBG").is_ok() {
                eprintln!(
                    "[crashdbg {}] code={code} stderr={}",
                    e.label(),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            return fail(msg);
        }
    }
    if c.xfail.contains(&e) {
        Status::Xpass
    } else {
        Status::Pass
    }
}
