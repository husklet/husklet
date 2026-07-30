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
//! The build environment selects the Linux linker + C compiler.
//!
//! STAGING PATH NOTE: the shims stage under `~/.hl/gl/<arch>/` (NOT the old `~/.hl/gui/<arch>/lib`), one
//! flat dir per arch, matching the CUDA driver's `~/.hl/cuda/<arch>/` layout.
//!
//! RECURSION GUARD: the nested shim build re-compiles THIS crate (the shim's `hl_gl` path dep), which
//! re-runs this build script. The `HL_GL_BUILDING_SHIM` sentinel (set on the child `cargo`) makes that
//! inner invocation a no-op, so there is no infinite recursion.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The egl shim crate: (crate subdir, built lib filename, deployed soname).
const SHIM_DIR: &str = "shim/egl";
const SHIM_LIB: &str = "libhl_egl_guest.so";
const EGL_SONAME: &str = "libEGL.so.1";
const GLES_SONAME: &str = "libGLESv2.so.2";
const WLEGL_SONAME: &str = "libwayland-egl.so.1";
const GBM_SONAME: &str = "libgbm.so.1";

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

    let manifest_dir = PathBuf::from(BuildEnvironment::required("CARGO_MANIFEST_DIR"));
    // Rerun when the shim's sources / manifest / this script / the forwarding stub change — AND when this
    // crate's own library sources change: the shim cdylib links `hl_gl` (this crate) as a path dependency,
    // so a change to the lowering/translator (e.g. `src/adapter/glsl.rs`, `src/service/frame.rs`) must
    // restage the guest shim, or the staged `libGLESv2.so.2` the e2e loads keeps the OLD lowering.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HL_DRIVER_STAGE");
    println!("cargo:rerun-if-env-changed=HL_DRIVER_ARCHES");
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
        manifest_dir.join("../../gpu/hl-gpu/src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("shim/wayland_egl.c").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("shim/gbm.c").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        vendor_dir(&manifest_dir).display()
    );

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let shim_target = manifest_dir.join("target").join("shim-build");
    let stage_root = stage_root(&manifest_dir);
    let sysroot = rustc_sysroot();
    let wlegl_c = manifest_dir.join("shim/wayland_egl.c");
    let gbm_c = manifest_dir.join("shim/gbm.c");
    let generated_include = generate_surface_header();

    for (triple, cc, arch_dir) in ARCHES {
        if !BuildEnvironment::selected(arch_dir) {
            continue;
        }
        let required = BuildEnvironment::mandatory(arch_dir);
        let cc = match *arch_dir {
            "aarch64" => std::env::var("HL_AARCH64_LINUX_CC").ok(),
            "x86_64" => std::env::var("HL_X86_64_LINUX_CC").ok(),
            _ => None,
        }
        .unwrap_or_else(|| (*cc).to_owned());
        if !BuildEnvironment::std_available(&sysroot, triple) {
            // aarch64 std is guaranteed on this host; a missing HOST std is a real, fail-loud error.
            if required {
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
        if let Err(e) = build_shim(&cargo, &manifest_dir, &egl_target, triple, &cc, "egl", None) {
            if required {
                panic!("building {SHIM_DIR} ({EGL_SONAME}) for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: building {EGL_SONAME} for {triple} failed: {e}");
            continue;
        }
        if let Err(e) = stage_lib(&egl_target, triple, &dst_dir, EGL_SONAME) {
            if required {
                panic!("staging {EGL_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: staging {EGL_SONAME} for {triple} failed: {e}");
            continue;
        }
        validate_exports(
            &cc,
            &dst_dir.join(EGL_SONAME),
            &manifest_dir.join("shim/egl/tests/golden/abi_symbols_egl.txt"),
            "egl",
        )
        .unwrap_or_else(|error| panic!("validating {EGL_SONAME} for {triple}: {error}"));
        // 2. Cross-build + stage libGLESv2.so.2 (the `gl*` object; `cfg(gles_client)`, DT_NEEDED on the
        //    just-staged libEGL.so.1 so it imports the shared-state accessor). Real gl* symbols in ITS
        //    OWN dynsym — matching real Mesa, so libepoxy resolves core gl* directly from it.
        let gles_target = shim_target.join("gles");
        if let Err(e) = build_shim(
            &cargo,
            &manifest_dir,
            &gles_target,
            triple,
            &cc,
            "gles",
            Some(&dst_dir),
        ) {
            if required {
                panic!("building {SHIM_DIR} ({GLES_SONAME}) for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: building {GLES_SONAME} for {triple} failed: {e}");
            continue;
        }
        if let Err(e) = stage_lib(&gles_target, triple, &dst_dir, GLES_SONAME) {
            if required {
                panic!("staging {GLES_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: staging {GLES_SONAME} for {triple} failed: {e}");
            continue;
        }
        validate_exports(
            &cc,
            &dst_dir.join(GLES_SONAME),
            &manifest_dir.join("shim/egl/tests/golden/abi_symbols_gl.txt"),
            "gl",
        )
        .unwrap_or_else(|error| panic!("validating {GLES_SONAME} for {triple}: {error}"));
        // 3. Compile + stage the libwayland-egl.so.1 wayland-egl ABI object (the app's `wl_egl_window`
        //    library). A SEPARATE object from libEGL — libEGL reads its `wl_egl_window` struct back.
        if let Err(e) = generate_wayland_egl(&cc, &wlegl_c, &dst_dir) {
            if required {
                panic!("generating {WLEGL_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: generating {WLEGL_SONAME} for {triple} failed: {e}");
            continue;
        }
        if let Err(e) = generate_gbm(&cc, &gbm_c, &generated_include, &dst_dir) {
            if required {
                panic!("generating {GBM_SONAME} for {triple}: {e}");
            }
            println!("cargo:warning=hl-gl: generating {GBM_SONAME} for {triple} failed: {e}");
            continue;
        }

        println!(
            "cargo:warning=hl-gl: staged guest GL shims for {triple} -> {}",
            dst_dir.display()
        );
    }
}

/// Require the staged ELF object to expose exactly its committed API surface. A hand-written entry point
/// without the role-specific `no_mangle` attribute otherwise compiles cleanly but fails at `dlsym` in GTK.
fn validate_exports(
    linker: &str,
    library: &Path,
    golden: &Path,
    prefix: &str,
) -> Result<(), String> {
    let nm = linker
        .strip_suffix("gcc")
        .map_or_else(|| "nm".to_owned(), |toolchain| format!("{toolchain}nm"));
    let output = Command::new(&nm)
        .args(["-D", "--defined-only"])
        .arg(library)
        .output()
        .map_err(|error| format!("spawn {nm}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{nm} exited with {}", output.status));
    }
    let actual = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .filter(|name| name.starts_with(prefix))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected = std::fs::read_to_string(golden)
        .map_err(|error| format!("read {}: {error}", golden.display()))?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if actual == expected {
        return Ok(());
    }
    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    let extra = actual.difference(&expected).cloned().collect::<Vec<_>>();
    Err(format!(
        "{} export surface differs from {}: missing={missing:?}, extra={extra:?}",
        library.display(),
        golden.display()
    ))
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
    let vendor = vendor_dir(manifest_dir);
    if !vendor.is_dir() {
        return Err(format!(
            "checked-in shim dependency source is missing: {}",
            vendor.display()
        ));
    }
    // The linker env var cargo reads for a target: CARGO_TARGET_<TRIPLE>_LINKER (triple upper-cased, - -> _).
    let linker_env = format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    );

    let mut cmd = Command::new(cargo);
    cmd.arg("build")
        .arg("--release")
        .arg("--offline")
        .arg("--config")
        .arg("source.crates-io.replace-with=\"vendored-sources\"")
        .arg("--config")
        .arg(format!("source.vendored-sources.directory={vendor:?}"))
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
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        // `cargo clippy` injects its driver through these variables. The nested command builds a target
        // artifact; it must not accidentally turn into a second, cross-target Clippy invocation.
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        // The parent Darwin shell contributes native linker flags (including `-lintl`). They are
        // invalid for Linux ELF and must not cross the target boundary.
        .env_remove("NIX_LDFLAGS")
        .env_remove("NIX_CFLAGS_COMPILE");
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

fn vendor_dir(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join("../../../vendor/rust/shim-deps")
}

/// Install the built `.so` as `<dst_dir>/<soname>` (+ an unversioned `lib*.so` symlink).
fn stage_lib(shim_target: &Path, triple: &str, dst_dir: &Path, soname: &str) -> Result<(), String> {
    let built = shim_target.join(triple).join("release").join(SHIM_LIB);
    std::fs::create_dir_all(dst_dir).map_err(|e| format!("mkdir {}: {e}", dst_dir.display()))?;
    let dst = dst_dir.join(soname);
    std::fs::copy(&built, &dst).map_err(|e| format!("copy -> {}: {e}", dst.display()))?;
    StagedLibrary::link_unversioned(dst_dir, soname);
    Ok(())
}

/// Compile the `libwayland-egl.so.1` wayland-egl ABI object from the one-file C shim (`DT_SONAME=
/// libwayland-egl.so.1`), linked against libc for `calloc`/`free`. Installs it next to libEGL (+
/// unversioned symlink). The app links `-lwayland-egl` and calls `wl_egl_window_create`; libEGL reads the
/// resulting struct back in `eglCreateWindowSurface`.
fn generate_wayland_egl(cc: &str, wlegl_c: &Path, dst_dir: &Path) -> Result<(), String> {
    generate_c_shim(cc, wlegl_c, dst_dir, WLEGL_SONAME)
}

fn generate_surface_header() -> PathBuf {
    use hl_surface_protocol::buffer;

    let include = PathBuf::from(BuildEnvironment::required("OUT_DIR"));
    let magic = buffer::MAGIC
        .iter()
        .map(|byte| format!("0x{byte:02x}"))
        .collect::<Vec<_>>()
        .join(", ");
    let header = format!(
        "#ifndef HL_SURFACE_BUFFER_H\n#define HL_SURFACE_BUFFER_H\n\
         #define HL_SURFACE_MAGIC {{{magic}}}\n\
         #define HL_SURFACE_VERSION {version}\n\
         #define HL_SURFACE_HEADER_LEN {header_len}\n\
         #define HL_SURFACE_PLANE_OFFSET {plane_offset}ULL\n\
         #define HL_SURFACE_MODIFIER 0x{modifier:016x}ULL\n\
         #endif\n",
        version = buffer::VERSION,
        header_len = buffer::HEADER_LEN,
        plane_offset = buffer::PLANE_OFFSET,
        modifier = buffer::MODIFIER,
    );
    std::fs::write(include.join("hl_surface_buffer.h"), header)
        .expect("write generated surface-buffer C header");
    include
}

fn generate_gbm(cc: &str, source: &Path, include: &Path, dst_dir: &Path) -> Result<(), String> {
    let out = dst_dir.join(GBM_SONAME);
    let status = Command::new(cc)
        .arg("-shared")
        .arg("-fPIC")
        .arg("-O2")
        .arg("-o")
        .arg(&out)
        .arg(source)
        .arg("-I")
        .arg(include)
        .arg("-L")
        .arg(dst_dir)
        .arg("-Wl,--no-as-needed")
        .arg(format!("-l:{EGL_SONAME}"))
        .arg("-Wl,--as-needed")
        .arg("-Wl,-rpath,$ORIGIN")
        .arg(format!("-Wl,-soname,{GBM_SONAME}"))
        .env_remove("NIX_LDFLAGS")
        .env_remove("NIX_CFLAGS_COMPILE")
        .status()
        .map_err(|e| format!("spawn {cc}: {e}"))?;
    if !status.success() {
        return Err(format!("{cc} link exited with {status}"));
    }
    if !out.exists() {
        return Err(format!("expected {} not produced", out.display()));
    }
    StagedLibrary::link_unversioned(dst_dir, GBM_SONAME);
    Ok(())
}

fn generate_c_shim(cc: &str, source: &Path, dst_dir: &Path, soname: &str) -> Result<(), String> {
    let out = dst_dir.join(soname);
    let status = Command::new(cc)
        .arg("-shared")
        .arg("-fPIC")
        .arg("-O2")
        .arg("-o")
        .arg(&out)
        .arg(source)
        .arg(format!("-Wl,-soname,{soname}"))
        .env_remove("NIX_LDFLAGS")
        .env_remove("NIX_CFLAGS_COMPILE")
        .status()
        .map_err(|e| format!("spawn {cc}: {e}"))?;
    if !status.success() {
        return Err(format!("{cc} link exited with {status}"));
    }
    if !out.exists() {
        return Err(format!("expected {} not produced", out.display()));
    }
    StagedLibrary::link_unversioned(dst_dir, soname);
    Ok(())
}

/// Create an unversioned `lib*.so -> <soname>` symlink some loaders/ldconfig setups want.
struct StagedLibrary;
impl StagedLibrary {
    fn link_unversioned(dst_dir: &Path, soname: &str) {
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
}

/// Revision-scoped application staging when packaging, otherwise crate-local build output.
fn stage_root(manifest_dir: &Path) -> PathBuf {
    std::env::var_os("HL_DRIVER_STAGE").map_or_else(
        || manifest_dir.join("../../../target/guest-drivers"),
        PathBuf::from,
    )
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
struct BuildEnvironment;
impl BuildEnvironment {
    fn selected(arch: &str) -> bool {
        std::env::var("HL_DRIVER_ARCHES")
            .map(|selected| selected.split(',').any(|value| value == arch))
            .unwrap_or(true)
    }

    fn mandatory(arch: &str) -> bool {
        arch == "aarch64" || std::env::var_os("HL_DRIVER_ARCHES").is_some()
    }

    fn std_available(sysroot: &Path, triple: &str) -> bool {
        sysroot
            .join("lib")
            .join("rustlib")
            .join(triple)
            .join("lib")
            .is_dir()
    }

    fn required(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| panic!("env {key} not set"))
    }
}
