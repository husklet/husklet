//! REAL SOFTWARE #3 — the REAL Khronos Vulkan loader drives OUR ICD from a real C program.
//!
//! We `gcc` `csrc/vk_compute.c` against the REAL Khronos Vulkan-Headers and link the REAL loader
//! (`libvulkan.so.1`), then run it with `VK_ICD_FILENAMES=~/.hl/vulkan/aarch64/icd.json` so the loader
//! loads OUR driver (`libvk_hl.so`) as the ICD, and `HL_GPU_EXEC` at our in-process host executor. The C
//! program creates an instance, enumerates physical devices, and drives create/bind/pipeline/dispatch/
//! submit — every call flowing loader → our ICD → socket → executor. We assert the program enumerated
//! OUR physical device by name ("hl Metal (Vulkan)") and completed the drive. See REALSOFTWARE.md for the
//! honest scope note (no device→host readback; the reference oracle records but doesn't run SPIR-V).

use std::process::Command;

mod common;
use common::{staged_dir, Executor};

const VK_HEADERS: &str = "/Users/x/vk-refs/Vulkan-Headers/include";
const LOADER: &str = "/usr/lib/aarch64-linux-gnu/libvulkan.so.1";
const DEVICE_NAME: &str = "hl Metal (Vulkan)";

#[test]
fn real_vulkan_loader_enumerates_our_icd_and_drives_it() {
    let vk_dir = staged_dir("vulkan");
    let icd = vk_dir.join("icd.json");
    assert!(icd.exists(), "staged ICD manifest missing at {icd:?}");
    assert!(vk_dir.join("libvk_hl.so").exists(), "staged libvk_hl.so missing at {vk_dir:?}");

    if !std::path::Path::new(LOADER).exists() {
        eprintln!("SKIP: real Vulkan loader {LOADER} not present");
        return;
    }
    if !std::path::Path::new(VK_HEADERS).exists() {
        eprintln!("SKIP: Vulkan-Headers not found at {VK_HEADERS}");
        return;
    }

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-vk-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_compute");

    // --- compile the REAL Vulkan C program against real headers ------------------------------------
    // It `dlopen`s the real loader at runtime (RTLD_LOCAL) rather than link-binding it — see the C
    // header comment + REALSOFTWARE.md for why (the staged ICD's default-visibility `vk*` exports
    // otherwise interpose the loader and self-deadlock it). Only `-ldl` is needed here.
    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_compute.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the Vulkan program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // --- host executor, then run the real program with the loader pointed at OUR ICD --------------
    let exec = Executor::start("vulkan");

    let run = Command::new(&bin)
        .env("VK_ICD_FILENAMES", &icd) // real loader → our driver as the ICD
        .env("VK_DRIVER_FILES", &icd) // newer loader env name (belt + suspenders)
        .env("VK_LOADER_LAYERS_DISABLE", "~all~") // no host implicit layers (Mesa device_select/anti_lag)
        .env("HL_GPU_EXEC", exec.sock())
        .env("VK_LOADER_DEBUG", "error,warn")
        .output()
        .expect("spawn vk_compute");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_compute stdout ---\n{stdout}\n--- vk_compute stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "real Vulkan C program exited non-zero (code {:?})",
        run.status.code()
    );
    // The real loader loaded our ICD and the program enumerated OUR physical device by name.
    assert!(
        stdout.contains(&format!("DEVICE_NAME: {DEVICE_NAME}")),
        "real Vulkan loader enumerated OUR device '{DEVICE_NAME}' through our ICD"
    );
    assert!(stdout.contains("PHYSICAL_DEVICE_COUNT: 1"), "our ICD reported exactly one device");
    assert!(stdout.contains("VK_DRIVE_OK"), "the program drove create/pipeline/dispatch/submit");
    // The guest actually drove our host executor over the socket.
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    let _ = std::fs::remove_dir_all(&out_dir);
}
