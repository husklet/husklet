//! Linux-engine guest compilation: static-PIE (`compile`) and static-non-PIE (`compile_nopie`) builds
//! from `guests/<source>`. aarch64 = native gcc, x86_64 = the cross compiler; both cached by mtime under
//! `cache/<arch>/` so the same source runs on both engines (what makes the engine matrix dense).
use super::*;

/// Compile a guest C source for a Linux engine. aarch64 = native gcc, x86_64 = the cross compiler; both
/// static-PIE, cached by mtime under cache/<arch>/. The same source runs on both engines (the point —
/// it makes the engine matrix dense). Returns the binary path.
pub(super) fn compile(ctx: &Ctx, source: &str, e: Engine) -> Result<String, String> {
    let src = ctx.guests.join(source);
    let out = ctx.cache.join(e.arch()).join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let needs = !out.exists()
        || std::fs::metadata(&src).and_then(|m| m.modified()).ok()
            >= std::fs::metadata(&out).and_then(|m| m.modified()).ok();
    if needs {
        // aarch64: native gcc + libsqlite3/libdl (real-software guests). x86_64: the cross compiler,
        // libm only (no x86 libsqlite3 on the dev host). Static, unused libs aren't pulled.
        let (cc, libs): (&str, &[&str]) = match e {
            Engine::LinuxAarch64 => ("gcc", &["-lsqlite3", "-lm", "-ldl"]),
            Engine::LinuxX86_64 => ("x86_64-linux-gnu-gcc", &["-lm"]),
            _ => return Err(format!("{} is not a compilable Linux target", e.label())),
        };
        let o = Command::new(cc)
            .args(["-O2", "-static-pie", "-pthread"])
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .args(libs)
            .output()
            .map_err(|err| format!("{cc} spawn: {err}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile {source} [{}]: {}",
                e.arch(),
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Compile a guest STATIC NON-PIE (`-static -no-pie` → ET_EXEC), so the loader biases it high and turns
/// on dispatch.c's non-PIE pointer-arg rebase (`g_nonpie_lo`). Cached under cache/<arch>/nopie/ so it
/// never collides with the same source's static-PIE build. Same native/qemu oracle as `compile`.
pub(super) fn compile_nopie(ctx: &Ctx, source: &str, e: Engine) -> Result<String, String> {
    let src = ctx.guests.join(source);
    let out = ctx
        .cache
        .join(e.arch())
        .join("nopie")
        .join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let needs = !out.exists()
        || std::fs::metadata(&src).and_then(|m| m.modified()).ok()
            >= std::fs::metadata(&out).and_then(|m| m.modified()).ok();
    if needs {
        let (cc, libs): (&str, &[&str]) = match e {
            Engine::LinuxAarch64 => ("gcc", &["-lm"]),
            Engine::LinuxX86_64 => ("x86_64-linux-gnu-gcc", &["-lm"]),
            _ => return Err(format!("{} is not a compilable Linux target", e.label())),
        };
        let o = Command::new(cc)
            .args(["-O2", "-static", "-no-pie", "-pthread"])
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .args(libs)
            .output()
            .map_err(|err| format!("{cc} spawn: {err}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile-nopie {source} [{}]: {}",
                e.arch(),
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
    }
    Ok(out.to_string_lossy().into_owned())
}
