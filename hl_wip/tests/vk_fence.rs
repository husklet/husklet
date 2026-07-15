//! VK SYNC DEMO — vk_fence: a real C Vulkan program submits a compute dispatch (`C[i] = A[i]*4 + 1`) with a
//! `VkFence`, blocks the host in `vkWaitForFences`, then the completed work is read back BIT-EXACT. The
//! guest proves the fence polls `VK_NOT_READY` before completion, `VK_SUCCESS` (signaled) after the wait,
//! returns to `VK_NOT_READY` after `vkResetFences`, and that a bad `VkFence` returns a real error. The
//! whole thing flows through the REAL `vkQueueSubmit` + `vkWaitForFences` shim lowering (loader → OUR ICD →
//! IR → WgpuExecutor/lavapipe); the host reads C back off the executor and asserts it is bit-exact,
//! proving the host observed the fence-completed work.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const VK_HEADERS: &str = "/Users/x/vk-refs/Vulkan-Headers/include";
const LOADER: &str = "/usr/lib/aarch64-linux-gnu/libvulkan.so.1";

const N: usize = 256;

fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

fn write_spv(path: &std::path::Path, words: &[u32]) {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    std::fs::write(path, &bytes).expect("write spir-v file");
}

fn decode_u32s(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[test]
fn real_vulkan_fence_host_observes_completed_work_bit_exact() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vkfence-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_fence");

    // C[i] = A[i]*4 + 1 (bindings 0 read = A, 1 write = C).
    let cs_spirv = wgsl_to_spirv(
        r#"
        @group(0) @binding(0) var<storage, read>       a: array<u32>;
        @group(0) @binding(1) var<storage, read_write> c: array<u32>;
        @compute @workgroup_size(64)
        fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
            let i = gid.x;
            c[i] = a[i] * 4u + 1u;
        }
    "#,
    );
    let cs_path = out_dir.join("vkfence.comp.spv");
    write_spv(&cs_path, &cs_spirv);

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_fence.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("vkfence");
    let adapter = exec.adapter_name();
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "must run on the software Vulkan device, got {adapter:?}"
    );

    let run = Command::new(&bin)
        .env("VK_ICD_FILENAMES", &icd)
        .env("VK_DRIVER_FILES", &icd)
        .env("VK_LOADER_LAYERS_DISABLE", "~all~")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_VK_CS_SPV", &cs_path)
        .env("VK_LOADER_DEBUG", "error,warn")
        .output()
        .expect("spawn vk_fence");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_fence stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    // The guest itself asserts the fence status transitions (NOT_READY → SUCCESS → NOT_READY) and that a
    // bad VkFence errors — a non-zero exit means one of those checks failed.
    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("VK_FENCE_OK"), "guest drove the fence-gated submit + host wait");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // A[i]=i+1 => C[i] = 4*(i+1) + 1 = 4i+5.
    let expected: Vec<u32> = (0..N as u32).map(|i| 4 * (i + 1) + 1).collect();

    let cap = exec.captured();
    let want_bytes = N * 4;
    let decoded: Vec<Vec<u32>> = cap
        .buffers
        .values()
        .filter(|v| v.len() == want_bytes)
        .map(|v| decode_u32s(v))
        .collect();

    let matched = decoded.iter().any(|g| *g == expected);
    assert!(
        matched,
        "fence-completed output C not found bit-exact. expected head {:?}, candidates {:?}",
        &expected[..8],
        decoded.iter().map(|c| c[..8.min(c.len())].to_vec()).collect::<Vec<_>>()
    );
}
