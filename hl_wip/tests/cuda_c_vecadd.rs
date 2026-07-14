//! REAL SOFTWARE #4 — a real C program drives the CUDA Driver API through our staged libcuda.so.
//!
//! At test time we `gcc` `csrc/cuda_vecadd.c` against a faithful minimal `cuda.h` and link `-lcuda` from
//! `~/.hl/cuda/aarch64`, then run the resulting native binary with `LD_LIBRARY_PATH` pointed there and
//! `HL_GPU_EXEC` at our in-process host executor's socket. The C program does a full CUDA Driver-API
//! vecadd; every `cu*` call enters our real shim, lowers, and ships over the socket to the reference
//! `CpuExecutor` (which compiles the PTX via the injected front-end and runs the kernel). We assert the C
//! program exited 0 and printed the correct elementwise sum — a real, separately-compiled CUDA program
//! (not our Rust harness) computing through `libcuda.so.1`.

use std::process::Command;

mod common;
use common::{staged_dir, Executor};

#[test]
fn real_c_cuda_program_computes_vecadd_through_libcuda() {
    let cuda_dir = staged_dir("cuda");
    assert!(
        cuda_dir.join("libcuda.so").exists() || cuda_dir.join("libcuda.so.1").exists(),
        "staged libcuda missing at {cuda_dir:?} — build hl_wip-cuda's shim first"
    );

    let manifest = env!("CARGO_MANIFEST_DIR");
    let csrc = format!("{manifest}/csrc/cuda_vecadd.c");
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-cuda-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("cuda_vecadd");

    // --- compile the REAL C program against libcuda ------------------------------------------------
    let compile = Command::new("gcc")
        .arg(&csrc)
        .arg("-I")
        .arg(format!("{manifest}/csrc"))
        .arg("-L")
        .arg(&cuda_dir)
        .arg("-lcuda")
        .arg("-o")
        .arg(&bin)
        // rpath so the runtime resolves OUR libcuda even without LD_LIBRARY_PATH; we also set it below.
        .arg(format!("-Wl,-rpath,{}", cuda_dir.display()))
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the CUDA program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // --- stand up the host executor, then run the real binary --------------------------------------
    let exec = Executor::start("cuda");

    let run = Command::new(&bin)
        .env("LD_LIBRARY_PATH", &cuda_dir)
        .env("HL_GPU_EXEC", exec.sock())
        .output()
        .expect("spawn cuda_vecadd");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- cuda_vecadd stdout ---\n{stdout}\n--- cuda_vecadd stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "real CUDA C program exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(stdout.contains("RESULT: 11.0 22.0 33.0 44.0"), "C program read back a + b over the socket");
    assert!(stdout.contains("VECADD_OK"), "C program asserted the sum itself");
    // The guest actually drove our executor (not a silently-stubbed success).
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    let _ = std::fs::remove_dir_all(&out_dir);
}
