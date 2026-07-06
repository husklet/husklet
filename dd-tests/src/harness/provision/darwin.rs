//! macOS/arm64 Mach-O guest compilation via the mac toolchain (native on macOS, over the `mac` bridge on
//! a Linux dev host). `compile_darwin` = a bare `-nostartfiles -e _start` guest (darwin syscall ABI, its
//! own sources); `compile_darwin_libc` = a *portable* POSIX source linked against the full libSystem, so
//! the same source that runs on the Linux engines also runs un-emulated under darwinjail.
use super::*;

/// Compile a static macOS/arm64 Mach-O guest from `guests/darwin/<source>` via the mac toolchain.
/// (Darwin guests use a different syscall ABI than linux, so they're their own sources; checked golden
/// since they can't run natively on a linux dev host for an oracle.)
pub(super) fn compile_darwin(ctx: &Ctx, source: &str) -> Result<String, String> {
    let src = ctx.guests.join("darwin").join(source);
    let out = ctx.cache.join("darwin").join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let fresh = std::fs::metadata(&out).and_then(|m| m.modified()).ok()
        >= std::fs::metadata(&src).and_then(|m| m.modified()).ok();
    if !out.exists() || !fresh {
        let script = format!(
            "clang -arch arm64 -nostartfiles -e _start -o '{}' '{}' -lSystem",
            out.display(),
            src.display()
        );
        let o = if cfg!(target_os = "macos") {
            Command::new("bash").arg("-lc").arg(&script).output()
        } else {
            Command::new("mac")
                .arg("bash")
                .arg("-lc")
                .arg(&script)
                .output()
        }
        .map_err(|e| format!("mac clang spawn: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile darwin/{source}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Compile a *portable* guest from `guests/<source>` as a normal macOS/arm64 Mach-O linked against the
/// full libSystem (real C runtime + main), cached under cache/darwin/. Runs natively under darwinjail —
/// so the same POSIX source that runs on the Linux engines also runs (un-emulated) on macOS.
pub(super) fn compile_darwin_libc(ctx: &Ctx, source: &str) -> Result<String, String> {
    let src = ctx.guests.join(source);
    let out = ctx.cache.join("darwin").join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let fresh = std::fs::metadata(&out).and_then(|m| m.modified()).ok()
        >= std::fs::metadata(&src).and_then(|m| m.modified()).ok();
    if !out.exists() || !fresh {
        let script = format!(
            "clang -arch arm64 -O2 -o '{}' '{}'",
            out.display(),
            src.display()
        );
        let o = if cfg!(target_os = "macos") {
            Command::new("bash").arg("-lc").arg(&script).output()
        } else {
            Command::new("mac")
                .arg("bash")
                .arg("-lc")
                .arg(&script)
                .output()
        }
        .map_err(|e| format!("mac clang spawn: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile darwin(libc) {source}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
    }
    Ok(out.to_string_lossy().into_owned())
}
