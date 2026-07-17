//! Cross-build the guest `libEGL.so.1` + `libGLESv2.so.2` shim cdylibs (`shim/egl`) for both guest
//! arches and stage the artifacts under `~/.hl/gl/<arch>/` where the [`crate::driver::Gl`] plug binds them.
//!
//! The ONE `shim/egl` crate is cross-built TWICE per arch, in two roles (selected via `$HL_SHIM_ROLE`),
//! matching real Mesa's `libGLESv2`/`libEGL` split:
//!   * `egl`  → `libEGL.so.1`   — exports the `egl*` set + the shared-state accessor `hl_shim_state_ptr`;
//!     OWNS the process-global `State`.
//!   * `gles` → `libGLESv2.so.2` — exports the `gl*` set in ITS OWN dynsym (so libepoxy resolves core
//!     `gl*` directly from it); built `cfg(gles_client)` with a `DT_NEEDED` on the just-staged
//!     `libEGL.so.1`, from which it imports `hl_shim_state_ptr` so BOTH objects share ONE `State`
//!     (`glDrawArrays` records the draw-list that `eglSwapBuffers` flushes).
//!
//! Each role is a nested `cargo build --release --target <triple>` into its own target subdir (so the two
//! builds don't thrash each other's cache); its build.rs bakes the DT_SONAME + a version script pinning
//! the exact exported-symbol set. Both `.so`s land beside each other, plus an unversioned `lib*.so`
//! symlink. A third small object, `libwayland-egl.so.1`, is compiled from a one-file C shim.
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
const WLEGL_SONAME: &str = "libwayland-egl.so.1";

/// (rust target triple, cross linker / C compiler, install-dir arch name).
const ARCHES: &[(&str, &str, &str)] = &[
    (
        "aarch64-unknown-linux-gnu",
        "aarch64-linux-gnu-gcc",
        "aarch64",
    ),
    ("x86_64-unknown-linux-gnu", "x86_64-linux-gnu-gcc", "x86_64"),
];

fn main() {
    // Recursion guard: when the nested shim build re-compiles this crate, do nothing.
    if std::env::var_os("HL_GL_BUILDING_SHIM").is_some() {
        return;
    }

    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    // Rerun when the shim's sources / manifest / this script / the forwarding stub change — AND when this
    // crate's own library sources change: the shim cdylib links `hl_gl` (this crate) as a path dependency,
    // so a change to the lowering/translator (e.g. `src/adapter/glsl.rs`, `src/service/frame.rs`) must
    // restage the guest shim, or the staged `libGLESv2.so.2` the e2e loads keeps the OLD lowering.
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(SHIM_DIR).join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(SHIM_DIR).join("registry").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(SHIM_DIR).join("build.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("shim/wayland-egl/wayland_egl.c")
            .display()
    );

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let shim_target = manifest_dir.join("target").join("shim-build");
    let stage_root = stage_root();
    let sysroot = rustc_sysroot();
    let wlegl_c = manifest_dir.join("shim/wayland-egl/wayland_egl.c");

    for (triple, cc, arch_dir) in ARCHES {
        let host = *triple == host_triple();
        if !std_available(&sysroot, triple) {
            // aarch64 std is guaranteed on this host; a missing HOST std is a real, fail-loud error.
            if host {
                panic!(
                    "host target std for {triple} is missing under {}",
                    sysroot.display()
                );
            }
            println!(
                "cargo:warning=hl-gl: rust std for {triple} not installed (no rustup on this host); \
                 skipping the x86_64 guest shim build — install it to stage x86_64 shims"
            );
            continue;
        }
        let dst_dir = stage_root.join("gl").join(arch_dir);

        // 1. Cross-build + stage libEGL.so.1 (the `egl*` object; OWNS the shared `State`). Each role uses
        //    its OWN target subdir so the two builds of this one crate don't thrash each other's cache.
        let egl_target = shim_target.join("egl");
        if let Err(e) = build_shim(&cargo, &manifest_dir, &egl_target, triple, cc, "egl", None) {
            if host {
                panic!("building {SHIM_DIR} ({EGL_SONAME}) for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: building {EGL_SONAME} for {triple} failed: {e}");
            continue;
        }
        if let Err(e) = stage_lib(&egl_target, triple, &dst_dir, EGL_SONAME) {
            if host {
                panic!("staging {EGL_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: staging {EGL_SONAME} for {triple} failed: {e}");
            continue;
        }
        // 2. Cross-build + stage libGLESv2.so.2 (the `gl*` object; `cfg(gles_client)`, DT_NEEDED on the
        //    just-staged libEGL.so.1 so it imports the shared-state accessor). Real gl* symbols in ITS
        //    OWN dynsym — matching real Mesa, so libepoxy resolves core gl* directly from it.
        let gles_target = shim_target.join("gles");
        if let Err(e) = build_shim(
            &cargo,
            &manifest_dir,
            &gles_target,
            triple,
            cc,
            "gles",
            Some(&dst_dir),
        ) {
            if host {
                panic!("building {SHIM_DIR} ({GLES_SONAME}) for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: building {GLES_SONAME} for {triple} failed: {e}");
            continue;
        }
        if let Err(e) = stage_lib(&gles_target, triple, &dst_dir, GLES_SONAME) {
            if host {
                panic!("staging {GLES_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: staging {GLES_SONAME} for {triple} failed: {e}");
            continue;
        }
        // 3. Compile + stage the libwayland-egl.so.1 wayland-egl ABI object (the app's `wl_egl_window`
        //    library). A SEPARATE object from libEGL — libEGL reads its `wl_egl_window` struct back.
        if let Err(e) = generate_wayland_egl(cc, &wlegl_c, &dst_dir) {
            if host {
                panic!("generating {WLEGL_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: generating {WLEGL_SONAME} for {triple} failed: {e}");
            continue;
        }

        println!(
            "cargo:warning=hl-gl: staged guest GL shims for {triple} -> {}",
            dst_dir.display()
        );
    }
}

/// Cross-build the shim crate for `triple` in the given `role` (`egl` or `gles`), with the recursion
/// sentinel + offline + the arch's cross linker. `egl_libdir` (Some only for the `gles` role) is the dir
/// holding the staged `libEGL.so.1` the libGLESv2 object `DT_NEEDED`s for the shared-state accessor.
fn build_shim(
    cargo: &str,
    manifest_dir: &Path,
    shim_target: &Path,
    triple: &str,
    linker: &str,
    role: &str,
    egl_libdir: Option<&Path>,
) -> Result<(), String> {
    let crate_manifest = manifest_dir.join(SHIM_DIR).join("Cargo.toml");
    // The linker env var cargo reads for a target: CARGO_TARGET_<TRIPLE>_LINKER (triple upper-cased, - -> _).
    let linker_env = format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    );

    let mut cmd = Command::new(cargo);
    cmd.arg("build")
        .arg("--release")
        .arg("--offline")
        .arg("--manifest-path")
        .arg(&crate_manifest)
        .arg("--target")
        .arg(triple)
        .arg("--target-dir")
        .arg(shim_target)
        .env("HL_GL_BUILDING_SHIM", "1")
        .env("HL_SHIM_ROLE", role)
        .env(&linker_env, linker)
        // Don't inherit the parent build's RUSTFLAGS (e.g. a host-only flag) into the guest cdylib.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if let Some(libdir) = egl_libdir {
        cmd.env("HL_SHIM_EGL_LIBDIR", libdir);
    }
    let status = cmd.status().map_err(|e| format!("spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build exited with {status}"));
    }
    let built = shim_target.join(triple).join("release").join(SHIM_LIB);
    if !built.exists() {
        return Err(format!(
            "expected artifact {} not produced",
            built.display()
        ));
    }
    Ok(())
}

/// Install the built `.so` as `<dst_dir>/<soname>` (+ an unversioned `lib*.so` symlink).
fn stage_lib(shim_target: &Path, triple: &str, dst_dir: &Path, soname: &str) -> Result<(), String> {
    let built = shim_target.join(triple).join("release").join(SHIM_LIB);
    std::fs::create_dir_all(dst_dir).map_err(|e| format!("mkdir {}: {e}", dst_dir.display()))?;
    let dst = dst_dir.join(soname);
    std::fs::copy(&built, &dst).map_err(|e| format!("copy -> {}: {e}", dst.display()))?;
    symlink_unversioned(dst_dir, soname);
    Ok(())
}

/// Compile the `libwayland-egl.so.1` wayland-egl ABI object from the one-file C shim (`DT_SONAME=
/// libwayland-egl.so.1`), linked against libc for `calloc`/`free`. Installs it next to libEGL (+
/// unversioned symlink). The app links `-lwayland-egl` and calls `wl_egl_window_create`; libEGL reads the
/// resulting struct back in `eglCreateWindowSurface`.
fn generate_wayland_egl(cc: &str, wlegl_c: &Path, dst_dir: &Path) -> Result<(), String> {
    let out = dst_dir.join(WLEGL_SONAME);
    let status = Command::new(cc)
        .arg("-shared")
        .arg("-fPIC")
        .arg("-O2")
        .arg("-o")
        .arg(&out)
        .arg(wlegl_c)
        .arg(format!("-Wl,-soname,{WLEGL_SONAME}"))
        .status()
        .map_err(|e| format!("spawn {cc}: {e}"))?;
    if !status.success() {
        return Err(format!("{cc} link exited with {status}"));
    }
    if !out.exists() {
        return Err(format!("expected {} not produced", out.display()));
    }
    symlink_unversioned(dst_dir, WLEGL_SONAME);
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
    sysroot
        .join("lib")
        .join("rustlib")
        .join(triple)
        .join("lib")
        .is_dir()
}

/// The build host's target triple (`cargo` sets `HOST` for build scripts).
fn host_triple() -> String {
    std::env::var("HOST").unwrap_or_default()
}

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("env {k} not set"))
}
