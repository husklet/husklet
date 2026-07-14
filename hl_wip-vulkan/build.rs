//! Cross-build the guest Vulkan ICD shim cdylib (`shim/vulkan`) for both guest arches and stage the
//! artifacts under `~/.hl/vulkan/<arch>/` where the [`crate::driver::Vulkan`] plug binds them.
//!
//! Per arch it runs a nested `cargo build --release --target <triple>` of the shim crate into a
//! dedicated target dir, then installs the resulting `.so` under its guest soname + drops the driver
//! manifest:
//!   * `~/.hl/vulkan/<arch>/libvk_hl.so.1`  (from `shim/vulkan`, DT_SONAME baked by that crate's build.rs)
//!   * `~/.hl/vulkan/<arch>/libvk_hl.so`    (unversioned symlink — the icd.json `library_path` target)
//!   * `~/.hl/vulkan/<arch>/icd.json`       (copied from `shim/vulkan/icd.json`)
//!
//! HOST NOTE: only the aarch64 rust std is installed here (system rust, no rustup), so the aarch64 build
//! MUST succeed (a failure fails this build). For x86_64 the build is ATTEMPTED, but if the target std
//! is missing it emits a `cargo:warning` and skips gracefully — it never fails the build. Cross linkers
//! `aarch64-linux-gnu-gcc` / `x86_64-linux-gnu-gcc` select the right linker per arch.
//!
//! RECURSION GUARD: the nested shim build re-compiles THIS crate (the shim's `hl_vulkan` path dep),
//! which re-runs this build script. The `HL_VULKAN_BUILDING_SHIM` sentinel (set on the child `cargo`)
//! makes that inner invocation a no-op, so there is no infinite recursion.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The single guest ICD shim: (crate subdir, built lib filename, deployed soname).
const SHIM_DIR: &str = "shim/vulkan";
const SHIM_LIB: &str = "libvk_hl_guest.so";
const SHIM_SONAME: &str = "libvk_hl.so.1";

/// (rust target triple, cross linker, install-dir arch name).
const ARCHES: &[(&str, &str, &str)] = &[
    ("aarch64-unknown-linux-gnu", "aarch64-linux-gnu-gcc", "aarch64"),
    ("x86_64-unknown-linux-gnu", "x86_64-linux-gnu-gcc", "x86_64"),
];

fn main() {
    // Recursion guard: when the nested shim build re-compiles this crate, do nothing.
    if std::env::var_os("HL_VULKAN_BUILDING_SHIM").is_some() {
        return;
    }

    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    // Rerun only when the shim's sources / manifest / this script / the icd.json change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", manifest_dir.join(SHIM_DIR).join("src").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join(SHIM_DIR).join("registry").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join(SHIM_DIR).join("build.rs").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join(SHIM_DIR).join("icd.json").display());

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let shim_target = manifest_dir.join("target").join("shim-build");
    let stage_root = stage_root();
    let sysroot = rustc_sysroot();

    for (triple, linker, arch_dir) in ARCHES {
        let host = *triple == host_triple();
        if !std_available(&sysroot, triple) {
            // aarch64 std is guaranteed on this host; a missing HOST std is a real, fail-loud error.
            if host {
                panic!("host target std for {triple} is missing under {}", sysroot.display());
            }
            println!(
                "cargo:warning=hl_wip-vulkan: rust std for {triple} not installed (no rustup on this host); \
                 skipping the x86_64 guest ICD build — install it to stage x86_64 shims"
            );
            continue;
        }

        match build_shim(&cargo, &manifest_dir, &shim_target, triple, linker) {
            Ok(()) => match stage(&manifest_dir, &shim_target, &stage_root, triple, arch_dir) {
                Ok(()) => println!(
                    "cargo:warning=hl_wip-vulkan: staged guest ICD for {triple} -> {}",
                    stage_root.display()
                ),
                Err(e) => {
                    if host {
                        panic!("staging {SHIM_SONAME} for {triple}: {e}");
                    }
                    println!("cargo:warning=hl_wip-vulkan: staging {SHIM_SONAME} for {triple} failed: {e}");
                }
            },
            Err(e) => {
                if host {
                    panic!("building {SHIM_DIR} for {triple}: {e}");
                }
                println!("cargo:warning=hl_wip-vulkan: building {SHIM_DIR} for {triple} failed: {e}");
            }
        }
    }
}

/// Cross-build the shim crate for `triple`, with the recursion sentinel + offline + the arch's linker.
fn build_shim(
    cargo: &str,
    manifest_dir: &Path,
    shim_target: &Path,
    triple: &str,
    linker: &str,
) -> Result<(), String> {
    let crate_manifest = manifest_dir.join(SHIM_DIR).join("Cargo.toml");
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
        .env("HL_VULKAN_BUILDING_SHIM", "1")
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

/// Install the built `.so` under `<stage_root>/vulkan/<arch>/<soname>` (+ an unversioned symlink) and
/// drop the driver `icd.json` beside it.
fn stage(
    manifest_dir: &Path,
    shim_target: &Path,
    stage_root: &Path,
    triple: &str,
    arch_dir: &str,
) -> Result<(), String> {
    let built = shim_target.join(triple).join("release").join(SHIM_LIB);
    let dst_dir = stage_root.join("vulkan").join(arch_dir);
    std::fs::create_dir_all(&dst_dir).map_err(|e| format!("mkdir {}: {e}", dst_dir.display()))?;

    let dst = dst_dir.join(SHIM_SONAME);
    std::fs::copy(&built, &dst).map_err(|e| format!("copy -> {}: {e}", dst.display()))?;

    // Unversioned symlink (libvk_hl.so -> libvk_hl.so.1) — the icd.json `library_path: ./libvk_hl.so`.
    if let Some(unversioned) = SHIM_SONAME.strip_suffix(".1") {
        let link = dst_dir.join(unversioned);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(SHIM_SONAME, &link);
        }
    }

    // The driver manifest the Vulkan loader reads (VK_ICD_FILENAMES points at it in the guest).
    let icd_src = manifest_dir.join(SHIM_DIR).join("icd.json");
    let icd_dst = dst_dir.join("icd.json");
    std::fs::copy(&icd_src, &icd_dst).map_err(|e| format!("copy icd.json -> {}: {e}", icd_dst.display()))?;
    Ok(())
}

/// `~/.hl` (or `/root/.hl` if `$HOME` is unset).
fn stage_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    Path::new(&home).join(".hl")
}

/// `rustc --print sysroot`.
fn rustc_sysroot() -> PathBuf {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    match Command::new(rustc).arg("--print").arg("sysroot").output() {
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
