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
//! The build environment selects the Linux linker.
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
    (
        "aarch64-unknown-linux-gnu",
        "aarch64-linux-gnu-gcc",
        "aarch64",
    ),
    ("x86_64-unknown-linux-gnu", "x86_64-linux-gnu-gcc", "x86_64"),
];

fn main() {
    // Recursion guard: when the nested shim build re-compiles this crate, do nothing.
    if std::env::var_os("HL_VULKAN_BUILDING_SHIM").is_some() {
        return;
    }

    let manifest_dir = PathBuf::from(BuildEnvironment::required("CARGO_MANIFEST_DIR"));
    // Rerun when the shim's sources / manifest / this script / the icd.json change — AND when this
    // crate's own `src/` changes: the guest ICD cdylib links this crate (e.g. the shim's
    // `vkEnumerateInstanceExtensionProperties` reads `capability::INSTANCE_EXTENSIONS`), so a change to
    // `src/` must restage the guest ICD or the staged `.so` goes stale (a real bug: an added instance
    // extension never reached the loader until the shim src happened to change).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HL_DRIVER_STAGE");
    println!("cargo:rerun-if-env-changed=HL_DRIVER_ARCHES");
    println!("cargo:rerun-if-changed=Cargo.toml");
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
        manifest_dir.join(SHIM_DIR).join("build").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(SHIM_DIR).join("build.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(SHIM_DIR).join("icd.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        vendor_dir(&manifest_dir).display()
    );

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let shim_target = manifest_dir.join("target").join("shim-build");
    let stage_root = stage_root(&manifest_dir);
    let sysroot = rustc_sysroot();

    for (triple, linker, arch_dir) in ARCHES {
        if !BuildEnvironment::selected(arch_dir) {
            continue;
        }
        let required = BuildEnvironment::mandatory(arch_dir);
        let linker = match *arch_dir {
            "aarch64" => std::env::var("HL_AARCH64_LINUX_CC").ok(),
            "x86_64" => std::env::var("HL_X86_64_LINUX_CC").ok(),
            _ => None,
        }
        .unwrap_or_else(|| (*linker).to_owned());
        if !BuildEnvironment::std_available(&sysroot, triple) {
            // aarch64 std is guaranteed on this host; a missing HOST std is a real, fail-loud error.
            if required {
                panic!(
                    "host target std for {triple} is missing under {}",
                    sysroot.display()
                );
            }
            println!(
                "cargo:warning=hl-vulkan: rust std for {triple} not installed (no rustup on this host); \
                 skipping the x86_64 guest ICD build — install it to stage x86_64 shims"
            );
            continue;
        }

        match build_shim(&cargo, &manifest_dir, &shim_target, triple, &linker) {
            Ok(()) => {
                match stage(&manifest_dir, &shim_target, &stage_root, triple, arch_dir) {
                    Ok(()) => println!(
                        "cargo:warning=hl-vulkan: staged guest ICD for {triple} -> {}",
                        stage_root.display()
                    ),
                    Err(e) => {
                        if required {
                            panic!("staging {SHIM_SONAME} for {triple}: {e}");
                        }
                        println!("cargo:warning=hl-vulkan: staging {SHIM_SONAME} for {triple} failed: {e}");
                    }
                }
            }
            Err(e) => {
                if required {
                    panic!("building {SHIM_DIR} for {triple}: {e}");
                }
                println!("cargo:warning=hl-vulkan: building {SHIM_DIR} for {triple} failed: {e}");
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
    let vendor = vendor_dir(manifest_dir);
    if !vendor.is_dir() {
        return Err(format!(
            "checked-in shim dependency source is missing: {}",
            vendor.display()
        ));
    }
    let linker_env = format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    );

    let status = Command::new(cargo)
        .arg("build")
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
        .env("HL_VULKAN_BUILDING_SHIM", "1")
        .env(&linker_env, linker)
        // Don't inherit the parent build's RUSTFLAGS (e.g. a host-only flag) into the guest cdylib.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .env_remove("NIX_LDFLAGS")
        .env_remove("NIX_CFLAGS_COMPILE")
        .status()
        .map_err(|e| format!("spawn cargo: {e}"))?;
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
    std::fs::copy(&icd_src, &icd_dst)
        .map_err(|e| format!("copy icd.json -> {}: {e}", icd_dst.display()))?;
    Ok(())
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
    match Command::new(rustc).arg("--print").arg("sysroot").output() {
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
