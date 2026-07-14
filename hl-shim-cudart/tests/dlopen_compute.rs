//! Real-`.so` execution proof for the CUDA **runtime** library: the deployed `libcudart.so` (the
//! cdylib a guest `dlopen`s / links as `-lcudart`) must run a PTX vector-add end to end and read back
//! arithmetically correct results, driven by the checked-in C program `tests/compute.c` exactly as an
//! unmodified CUDA runtime app does. The lib unit test `vecadd_executes_end_to_end_through_cudart`
//! drives the same path through the extern "C" symbols directly.
//!
//! Skips if the cdylib is not built (`cargo build -p hl-shim-cudart` produces it) or no C toolchain.

use std::path::PathBuf;
use std::process::Command;

fn cdylib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile = exe.parent()?.parent()?;
    for name in ["libhl_shim_cudart.so", "libhl_shim_cudart.dylib"] {
        let p = profile.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[test]
fn deployed_libcudart_so_runs_vecadd_end_to_end() {
    let Some(lib) = cdylib() else {
        eprintln!("[dlopen_compute] libcudart cdylib not built (run `cargo build -p hl-shim-cudart`); skipping");
        return;
    };
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/compute.c");
    let out = std::env::temp_dir().join(format!("dd-cudart-compute-{}", std::process::id()));
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
        "the deployed libcudart.so failed the vecadd end-to-end run (exit {:?}).\nstdout: {stdout}\nstderr: {stderr}",
        run.status.code()
    );
    assert!(stdout.contains("compute OK"), "compute.c did not confirm a correct vecadd: {stdout:?}");
}
