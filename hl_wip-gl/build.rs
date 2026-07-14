//! Cross-build the guest `libEGL.so.1` shim cdylib (`shim/egl`) for both guest arches, synthesize the
//! thin `libGLESv2.so.2` forwarding stub, and stage the artifacts under `~/.hl/gl/<arch>/` where the
//! [`crate::driver::Gl`] plug binds them.
//!
//! Per arch it runs a nested `cargo build --release --target <triple>` of the egl shim into a dedicated
//! target dir, installs the resulting `.so` as `~/.hl/gl/<arch>/libEGL.so.1` (DT_SONAME baked by that
//! crate's build.rs), then links a minimal `libGLESv2.so.2` — an empty translation unit
//! (`shim/gles/forward.c`) with `DT_SONAME=libGLESv2.so.2` and a `DT_NEEDED` on `libEGL.so.1` (kept via
//! `--no-as-needed`), so a guest app that `DT_NEEDED`s libGLESv2.so.2 resolves every `gl*` symbol back to
//! the primary libEGL object. Both land beside each other, plus an unversioned `lib*.so` symlink.
//!
//! HOST NOTE: only the aarch64 rust std is installed here (system rust, no rustup), so the aarch64 build
//! MUST succeed (a failure fails this build). For x86_64 the build is ATTEMPTED, but if the target std is
//! missing it emits a `cargo:warning` and skips gracefully — it never fails the build. Cross linkers
//! `aarch64-linux-gnu-gcc` / `x86_64-linux-gnu-gcc` select the right linker + C compiler per arch.
//!
//! STAGING PATH NOTE: the shims stage under `~/.hl/gl/<arch>/` (NOT the old `~/.hl/gui/<arch>/lib`), one
//! flat dir per arch, matching the CUDA driver's `~/.hl/cuda/<arch>/` layout.
//!
//! RECURSION GUARD: the nested shim build re-compiles THIS crate (the shim's `hl_gl` path dep), which
//! re-runs this build script. The `HL_GL_BUILDING_SHIM` sentinel (set on the child `cargo`) makes that
//! inner invocation a no-op, so there is no infinite recursion.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The egl shim crate: (crate subdir, built lib filename, deployed soname).
const SHIM_DIR: &str = "shim/egl";
const SHIM_LIB: &str = "libhl_egl_guest.so";
const EGL_SONAME: &str = "libEGL.so.1";
const GLES_SONAME: &str = "libGLESv2.so.2";

/// (rust target triple, cross linker / C compiler, install-dir arch name).
const ARCHES: &[(&str, &str, &str)] = &[
    ("aarch64-unknown-linux-gnu", "aarch64-linux-gnu-gcc", "aarch64"),
    ("x86_64-unknown-linux-gnu", "x86_64-linux-gnu-gcc", "x86_64"),
];

fn main() {
    // Recursion guard: when the nested shim build re-compiles this crate, do nothing.
    if std::env::var_os("HL_GL_BUILDING_SHIM").is_some() {
        return;
    }

    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    // Rerun only when the shim's sources / manifest / this script / the forwarding stub change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", manifest_dir.join(SHIM_DIR).join("src").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join(SHIM_DIR).join("registry").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join(SHIM_DIR).join("build.rs").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("shim/gles/forward.c").display());

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let shim_target = manifest_dir.join("target").join("shim-build");
    let stage_root = stage_root();
    let sysroot = rustc_sysroot();
    let forward_c = manifest_dir.join("shim/gles/forward.c");

    for (triple, cc, arch_dir) in ARCHES {
        let host = *triple == host_triple();
        if !std_available(&sysroot, triple) {
            // aarch64 std is guaranteed on this host; a missing HOST std is a real, fail-loud error.
            if host {
                panic!("host target std for {triple} is missing under {}", sysroot.display());
            }
            println!(
                "cargo:warning=hl_wip-gl: rust std for {triple} not installed (no rustup on this host); \
                 skipping the x86_64 guest shim build — install it to stage x86_64 shims"
            );
            continue;
        }

        // 1. Cross-build the libEGL shim cdylib.
        if let Err(e) = build_shim(&cargo, &manifest_dir, &shim_target, triple, cc) {
            if host {
                panic!("building {SHIM_DIR} for {triple}: {e}");
            }
            println!("cargo:warning=hl_wip-gl: building {SHIM_DIR} for {triple} failed: {e}");
            continue;
        }
        // 2. Stage libEGL.so.1.
        let dst_dir = stage_root.join("gl").join(arch_dir);
        if let Err(e) = stage_lib(&shim_target, triple, &dst_dir) {
            if host {
                panic!("staging {EGL_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl_wip-gl: staging {EGL_SONAME} for {triple} failed: {e}");
            continue;
        }
        // 3. Synthesize + stage the libGLESv2.so.2 DT_NEEDED->libEGL forwarding stub next to it.
        if let Err(e) = generate_gles_stub(cc, &forward_c, &dst_dir) {
            if host {
                panic!("generating {GLES_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl_wip-gl: generating {GLES_SONAME} for {triple} failed: {e}");
            continue;
        }

        println!("cargo:warning=hl_wip-gl: staged guest GL shims for {triple} -> {}", dst_dir.display());
    }
}

/// Cross-build the egl shim crate for `triple`, with the recursion sentinel + offline + the arch's cross
/// linker.
fn build_shim(
    cargo: &str,
    manifest_dir: &Path,
    shim_target: &Path,
    triple: &str,
    linker: &str,
) -> Result<(), String> {
    let crate_manifest = manifest_dir.join(SHIM_DIR).join("Cargo.toml");
    // The linker env var cargo reads for a target: CARGO_TARGET_<TRIPLE>_LINKER (triple upper-cased, - -> _).
    let linker_env = format!("CARGO_TARGET_{}_LINKER", triple.to_uppercase().replace('-', "_"));

    let status = Command::new(cargo)
        .arg("build")
        .arg("--release")
        .arg("--offline")
        .arg("--manifest-path")
        .arg(&crate_manifest)
        .arg("--target")
        .arg(triple)
        .arg("--target-dir")
        .arg(shim_target)
        .env("HL_GL_BUILDING_SHIM", "1")
        .env(&linker_env, linker)
        // Don't inherit the parent build's RUSTFLAGS (e.g. a host-only flag) into the guest cdylib.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .map_err(|e| format!("spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build exited with {status}"));
    }
    let built = shim_target.join(triple).join("release").join(SHIM_LIB);
    if !built.exists() {
        return Err(format!("expected artifact {} not produced", built.display()));
    }
    Ok(())
}

/// Install the built `.so` as `<dst_dir>/libEGL.so.1` (+ an unversioned `libEGL.so` symlink).
fn stage_lib(shim_target: &Path, triple: &str, dst_dir: &Path) -> Result<(), String> {
    let built = shim_target.join(triple).join("release").join(SHIM_LIB);
    std::fs::create_dir_all(dst_dir).map_err(|e| format!("mkdir {}: {e}", dst_dir.display()))?;
    let dst = dst_dir.join(EGL_SONAME);
    std::fs::copy(&built, &dst).map_err(|e| format!("copy -> {}: {e}", dst.display()))?;
    symlink_unversioned(dst_dir, EGL_SONAME);
    Ok(())
}

/// Link the thin `libGLESv2.so.2` forwarding stub from an empty C TU: `DT_SONAME=libGLESv2.so.2` +
/// a kept `DT_NEEDED` on the just-staged `libEGL.so.1`. Installs it next to libEGL (+ unversioned symlink).
fn generate_gles_stub(cc: &str, forward_c: &Path, dst_dir: &Path) -> Result<(), String> {
    let out = dst_dir.join(GLES_SONAME);
    let status = Command::new(cc)
        .arg("-shared")
        .arg("-fPIC")
        .arg("-nostdlib")
        .arg("-o")
        .arg(&out)
        .arg(forward_c)
        .arg(format!("-Wl,-soname,{GLES_SONAME}"))
        .arg(format!("-L{}", dst_dir.display()))
        .arg("-Wl,--no-as-needed")
        .arg("-l:libEGL.so.1")
        .arg("-Wl,--as-needed")
        .status()
        .map_err(|e| format!("spawn {cc}: {e}"))?;
    if !status.success() {
        return Err(format!("{cc} link exited with {status}"));
    }
    if !out.exists() {
        return Err(format!("expected {} not produced", out.display()));
    }
    symlink_unversioned(dst_dir, GLES_SONAME);
    Ok(())
}

/// Create an unversioned `lib*.so -> <soname>` symlink some loaders/ldconfig setups want.
fn symlink_unversioned(dst_dir: &Path, soname: &str) {
    // Strip the trailing `.N` version (libEGL.so.1 -> libEGL.so, libGLESv2.so.2 -> libGLESv2.so).
    if let Some(dot) = soname.rfind(".so.") {
        let unversioned = &soname[..dot + 3]; // include ".so"
        let link = dst_dir.join(unversioned);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(soname, &link);
        }
    }
}

/// `~/.hl` (or `/root/.hl` if `$HOME` is unset).
fn stage_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    Path::new(&home).join(".hl")
}

/// `rustc --print sysroot`.
fn rustc_sysroot() -> PathBuf {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = Command::new(rustc).arg("--print").arg("sysroot").output();
    match out {
        Ok(o) if o.status.success() => {
            PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
        _ => PathBuf::from("/"),
    }
}

/// Is the rust std library for `triple` installed under `sysroot`?
fn std_available(sysroot: &Path, triple: &str) -> bool {
    sysroot.join("lib").join("rustlib").join(triple).join("lib").is_dir()
}

/// The build host's target triple (`cargo` sets `HOST` for build scripts).
fn host_triple() -> String {
    std::env::var("HOST").unwrap_or_default()
}

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("env {k} not set"))
}
