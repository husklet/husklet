use std::path::PathBuf;
use std::process::Command;

use super::*;

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
    // the run; the fixture rootfs' /tmp is already scratch for the sh-based cases). Darwin keeps the
    // host path: darwinjail runs our own Mach-O natively and only arms the jail around it.
    let mut jail_copy: Option<(String, String)> = None; // (host file to clean up, in-guest argv[0])
    if let Some(rfs) = &rootfs {
        if !matches!(c.bin, Bin::InRootfs) && e != Engine::DarwinAarch64 {
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
    let mut cfg = ddjit::SpawnConfig::new(String::new(), rootfs_str.clone());
    cfg.lowers = c.lowers.clone();
    // .overlay(): inject the rootfs as its own lower so g_nlower>0 turns on the overlay open/lseek path
    // (linux engines only; darwin has no overlayfs). Reproduces overlay-only bugs like in the matrix.
    if c.overlay && !rootfs_str.is_empty() && e != Engine::DarwinAarch64 {
        cfg.lowers.push(rootfs_str.clone());
    }
    cfg.mem_max = c.mem_max;
    cfg.cpus = c.cpus;
    cfg.read_only = c.read_only;
    cfg.ulimits = c.ulimits.clone();
    // Untrusted-guest SENTRY split: bake DDJIT_UNTRUSTED=1 into the engine's launch env (via SpawnConfig's
    // `env`, which serializes into the `exec env …` prefix of the launch script — so it survives the `mac`
    // bridge that drops ambient env). DDJIT_SANDBOX is left unset on purpose (ring/forwarding, not Seatbelt).
    if c.untrusted {
        cfg.env.push(("DDJIT_UNTRUSTED".into(), "1".into()));
    }
    for (k, v) in &c.env {
        cfg.env.push((k.clone(), v.clone()));
    }
    let argv0 = jail_copy
        .as_ref()
        .map(|(_, g)| g.clone())
        .unwrap_or_else(|| guest.clone());
    cfg.argv = match &c.bin {
        Bin::InRootfs => c.args.clone(),
        _ => std::iter::once(argv0)
            .chain(c.args.iter().cloned())
            .collect(),
    };
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
    // Darwin (darwinjail) shares this run()/redirect path — the `> file` below binds to the darwin launch
    // script exactly as it does for linux — but the DRAIN FILE LOCATION needs two darwin-only adjustments,
    // or the capture silently drops to empty on the darwin engine:
    //   (1) Seatbelt. The darwinjail arms a Seatbelt profile (DD_SANDBOX, only WHEN a rootfs is set) whose
    //       body is `(deny file-write* (subpath "/")) (allow file-write* (subpath "<rootfs>") …)` — writes
    //       outside the rootfs are DENIED. A drain under target/dd-tests/stdout/ (i.e. under /Users) is
    //       outside that set, so a rootfs darwin case's guest write to it is refused → empty file. (On a
    //       host that already confines the process, e.g. OrbStack, sandbox_init fails and this is masked —
    //       but on a real mac the deny is live.) Fix: for a rootfs darwin case, drain INTO the rootfs's
    //       /tmp — a Seatbelt-allowed subpath that is on the shared tree (so this Linux runner reads it
    //       back by the same host path) and stays writable even under docker --read-only.
    //   (2) Filename collision. darwin/aarch64 and linux/aarch64 share e.arch()=="aarch64", so the
    //       `{name}_{arch}_{pid}` file would be the SAME for both engines within one runner process — tag
    //       the darwin drain with the OS too so its file can never be clobbered by the same-arch linux run.
    // The linux drain path is left byte-identical (the `else` arm below).
    let drain_file = if e == Engine::DarwinAarch64 && !rootfs_str.is_empty() {
        // rootfs armed → the Seatbelt profile only permits writes under the rootfs; /tmp is rw even RO.
        PathBuf::from(&rootfs_str).join("tmp").join(format!(
            "ddstdout_{}_{}_{}.out",
            c.name.replace('/', "_"),
            e.os(),
            std::process::id()
        ))
    } else if e == Engine::DarwinAarch64 {
        // bare darwin → no Seatbelt; keep the shared drain dir but OS-tag the name (no arch collision).
        ctx.cache.join("stdout").join(format!(
            "{}_{}_{}_{}.out",
            c.name.replace('/', "_"),
            e.os(),
            e.arch(),
            std::process::id()
        ))
    } else {
        ctx.cache.join("stdout").join(format!(
            "{}_{}_{}.out",
            c.name.replace('/', "_"),
            e.arch(),
            std::process::id()
        ))
    };
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

fn eval(
    chk: &Check,
    stdout: &str,
    code: i32,
    guest: &str,
    args: &[String],
    e: Engine,
) -> Result<(), String> {
    match chk {
        Check::Exit(want) => (code == *want)
            .then_some(())
            .ok_or_else(|| format!("exit {code} != {want}")),
        Check::Out(want) => (stdout == *want)
            .then_some(())
            .ok_or_else(|| format!("stdout {:?} != {:?}", stdout, want)),
        Check::OutHas(sub) => stdout
            .contains(sub)
            .then_some(())
            .ok_or_else(|| format!("stdout {:?} lacks {:?}", stdout, sub)),
        Check::Oracle => {
            // native ground truth: aarch64 runs directly; x86_64 runs under qemu-user.
            let o = match e {
                Engine::LinuxX86_64 => Command::new("timeout")
                    .arg("25")
                    .arg("qemu-x86_64")
                    .arg(guest)
                    .args(args)
                    .output(),
                _ => Command::new("timeout")
                    .arg("25")
                    .arg(guest)
                    .args(args)
                    .output(),
            }
            .map_err(|err| format!("oracle spawn: {err}"))?;
            let (eo, ec) = (strip_noise(&o.stdout), o.status.code().unwrap_or(-1));
            if eo != stdout || ec != code {
                Err(format!(
                    "oracle mismatch (jit {code}/{stdout:?} vs native {ec}/{eo:?})"
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// Single-quote a string for safe inclusion in the mac-side `bash -lc` launch script (used to append
/// the stdout-drain redirect target). Mirrors `SpawnConfig::shq`.
fn shq(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() {
        if c == '\'' {
            o.push_str("'\\''");
        } else {
            o.push(c);
        }
    }
    o.push('\'');
    o
}

/// Drop the JIT's diagnostic "unhandled syscall ..." lines so they don't pollute stdout checks.
fn strip_noise(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .lines()
        .filter(|l| !l.contains("unhandled syscall"))
        .collect::<Vec<_>>()
        .join("\n")
        + if b.ends_with(b"\n") && !b.is_empty() {
            "\n"
        } else {
            ""
        }
}
