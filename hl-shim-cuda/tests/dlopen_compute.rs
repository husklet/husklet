//! Real-`.so` execution proof: the DEPLOYED `libcuda.so` (the cdylib a guest `dlopen`s as its CUDA
//! driver) must run a PTX vector-add end to end and read back arithmetically correct results. The lib
//! unit test `vecadd_executes_end_to_end_through_the_shim` drives the same path through the extern "C"
//! symbols directly; this drives the built shared object exactly as an unmodified CUDA app does —
//! `dlopen(libcuda.so)` + `dlsym(cuMemAlloc/cuModuleLoadData/cuLaunchKernel/…)` — via the checked-in C
//! program `tests/compute.c`.
//!
//! Skips (does not fail) if the cdylib is not present (a bare `cargo test` builds the rlib + test
//! binaries but not the cdylib; a prior `cargo build -p hl-shim-cuda`, as the deploy/CI does, produces
//! it) or if no C toolchain is available. Nested `cargo build` is deliberately avoided — it would
//! deadlock on the outer test run's `target/` lock.

use std::path::PathBuf;
use std::process::Command;

/// The deployed cdylib next to the test binary's profile dir (`target/<profile>/libhl_shim_cuda.{so,dylib}`).
fn cdylib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?; // target/<profile>/deps/<test>-<hash>
    let profile = exe.parent()?.parent()?; // target/<profile>
    for name in ["libhl_shim_cuda.so", "libhl_shim_cuda.dylib"] {
        let p = profile.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[test]
fn deployed_libcuda_so_runs_vecadd_end_to_end() {
    let Some(lib) = cdylib() else {
        eprintln!("[dlopen_compute] libcuda cdylib not built (run `cargo build -p hl-shim-cuda`); skipping");
        return;
    };
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/compute.c");
    let out = std::env::temp_dir().join(format!("hl-cuda-compute-{}", std::process::id()));
    let built = Command::new("cc")
        .arg(src)
        .args(["-ldl", "-lm", "-O1", "-o"])
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !built {
        eprintln!("[dlopen_compute] compute.c failed to compile (no C toolchain?); skipping");
        return;
    }
    let run = Command::new(&out).arg(&lib).output().expect("run compute.c");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "the deployed libcuda.so failed the vecadd end-to-end run (exit {:?}).\nstdout: {stdout}\nstderr: {stderr}",
        run.status.code()
    );
    assert!(
        stdout.contains("compute OK"),
        "compute.c did not confirm a correct vecadd: {stdout:?}"
    );
}
