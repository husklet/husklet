//! Cross-build the three guest shim cdylibs (`shim/cuda`, `shim/cudart`, `shim/nvml`) for both guest
//! arches and stage the artifacts under `~/.hl/` where the [`crate::driver::Cuda`] plug binds them.
//!
//! Per arch it runs a nested `cargo build --release --target <triple>` of each shim crate into a
//! dedicated target dir, then installs the resulting `.so` under its guest soname:
//!   * `~/.hl/cuda/<arch>/libcuda.so.1`     (from `shim/cuda`, DT_SONAME baked by that crate's build.rs)
//!   * `~/.hl/cuda/<arch>/libcudart.so.1`   (from `shim/cudart`)
//!   * `~/.hl/nvml/<arch>/libnvidia-ml.so.1` (from `shim/nvml`)
//! plus an unversioned `lib*.so` symlink some loaders want.
//!
//! HOST NOTE: only the aarch64 rust std is installed here (system rust, no rustup), so the aarch64 build
//! MUST succeed (a failure fails this build). For x86_64 the build is ATTEMPTED, but if the target std is
//! missing it emits a `cargo:warning` and skips gracefully — it never fails the build. Cross linkers
//! `aarch64-linux-gnu-gcc` / `x86_64-linux-gnu-gcc` select the right linker per arch.
//!
//! RECURSION GUARD: the nested shim build re-compiles THIS crate (the shim's `hl_cuda` path dep), which
//! re-runs this build script. The `HL_CUDA_BUILDING_SHIM` sentinel (set on the child `cargo`) makes that
//! inner invocation a no-op, so there is no infinite recursion.

use std::path::{Path, PathBuf};
use std::process::Command;

/// One shim crate: (crate subdir, built lib filename, install family, deployed soname).
struct Shim {
    dir: &'static str,
    lib: &'static str,
    family: &'static str,
    soname: &'static str,
}

const SHIMS: &[Shim] = &[
    Shim {
        dir: "shim/cuda",
        lib: "libhl_cuda_guest.so",
        family: "cuda",
        soname: "libcuda.so.1",
    },
    Shim {
        dir: "shim/cudart",
        lib: "libhl_cudart_guest.so",
        family: "cuda",
        soname: "libcudart.so.1",
    },
    Shim {
        dir: "shim/nvml",
        lib: "libhl_nvml_guest.so",
        family: "nvml",
        soname: "libnvidia-ml.so.1",
    },
];

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
    if std::env::var_os("HL_CUDA_BUILDING_SHIM").is_some() {
        return;
    }

    let manifest_dir = PathBuf::from(BuildEnvironment::required("CARGO_MANIFEST_DIR"));
    // Rerun only when a shim's sources / manifest / this script change (keeps repeat `cargo test` cheap).
    println!("cargo:rerun-if-changed=build.rs");
    for s in SHIMS {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(s.dir).join("src").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(s.dir).join("registry").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(s.dir).join("build.rs").display()
        );
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let shim_target = manifest_dir.join("target").join("shim-build");
    let stage_root = stage_root();
    let sysroot = rustc_sysroot();

    for (triple, linker, arch_dir) in ARCHES {
        let host = *triple == host_triple();
        if !BuildEnvironment::std_available(&sysroot, triple) {
            // aarch64 std is guaranteed on this host; a missing HOST std is a real, fail-loud error.
            if host {
                panic!(
                    "host target std for {triple} is missing under {}",
                    sysroot.display()
                );
            }
            println!(
                "cargo:warning=hl-cuda: rust std for {triple} not installed (no rustup on this host); \
                 skipping the x86_64 guest shim build — install it to stage x86_64 shims"
            );
            continue;
        }

        let mut built_ok = true;
        for s in SHIMS {
            match build_shim(&cargo, &manifest_dir, &shim_target, s, triple, linker) {
                Ok(()) => {
                    if let Err(e) = stage(&shim_target, &stage_root, s, triple, arch_dir) {
                        if host {
                            panic!("staging {} for {triple}: {e}", s.soname);
                        }
                        println!(
                            "cargo:warning=hl-cuda: staging {} for {triple} failed: {e}",
                            s.soname
                        );
                        built_ok = false;
                    }
                }
                Err(e) => {
                    if host {
                        panic!("building {} for {triple}: {e}", s.dir);
                    }
                    println!(
                        "cargo:warning=hl-cuda: building {} for {triple} failed: {e}",
                        s.dir
                    );
                    built_ok = false;
                    break;
                }
            }
        }
        if built_ok {
            println!(
                "cargo:warning=hl-cuda: staged guest shims for {triple} -> {}",
                stage_root.display()
            );
        }
    }
}

/// Cross-build one shim crate for `triple`, with the recursion sentinel + offline (no network here) +
/// the arch's cross linker.
fn build_shim(
    cargo: &str,
    manifest_dir: &Path,
    shim_target: &Path,
    shim: &Shim,
    triple: &str,
    linker: &str,
) -> Result<(), String> {
    let crate_manifest = manifest_dir.join(shim.dir).join("Cargo.toml");
    // The linker env var cargo reads for a target: CARGO_TARGET_<TRIPLE>_LINKER (triple upper-cased, - -> _).
    let linker_env = format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    );

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
        .env("HL_CUDA_BUILDING_SHIM", "1")
        .env(&linker_env, linker)
        // Don't inherit the parent build's RUSTFLAGS (e.g. a host-only flag) into the guest cdylib.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .map_err(|e| format!("spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build exited with {status}"));
    }
    let built = shim_target.join(triple).join("release").join(shim.lib);
    if !built.exists() {
        return Err(format!(
            "expected artifact {} not produced",
            built.display()
        ));
    }
    Ok(())
}

/// Install the built `.so` under `<stage_root>/<family>/<arch>/<soname>` (+ an unversioned symlink).
fn stage(
    shim_target: &Path,
    stage_root: &Path,
    shim: &Shim,
    triple: &str,
    arch_dir: &str,
) -> Result<(), String> {
    let built = shim_target.join(triple).join("release").join(shim.lib);
    let dst_dir = stage_root.join(shim.family).join(arch_dir);
    std::fs::create_dir_all(&dst_dir).map_err(|e| format!("mkdir {}: {e}", dst_dir.display()))?;
    let dst = dst_dir.join(shim.soname);
    std::fs::copy(&built, &dst).map_err(|e| format!("copy -> {}: {e}", dst.display()))?;

    // Unversioned symlink (libcuda.so -> libcuda.so.1) some loaders/ldconfig setups want.
    if let Some(unversioned) = shim.soname.strip_suffix(".1") {
        let link = dst_dir.join(unversioned);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(shim.soname, &link);
        }
    }
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

/// The build host's target triple (`cargo` sets `HOST` for build scripts).
fn host_triple() -> String {
    std::env::var("HOST").unwrap_or_default()
}
