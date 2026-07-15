//! VK COMPUTE DEMO — vk_dispatch_indirect: a real C Vulkan program sources every compute workgroup
//! dimension from a VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT buffer (a `VkDispatchIndirectCommand{x=4,y=1,z=1}`)
//! and issues ONE `vkCmdDispatchIndirect(indbuf, 0)` over the same saxpy shader as vk_compute_saxpy
//! (`C[i] = A[i]*3 + B[i]`, 256 elements). This is the regression guard for the shim reading the indirect
//! argument buffer and lowering the buffer-sourced dims to the SAME `Enc::Dispatch` the direct
//! `vkCmdDispatch(4,1,1)` would emit — before the fix `vkCmdDispatchIndirect` was a validated no-op (no
//! dispatch reached lavapipe → C stayed unwritten). The host reads C back off the executor and asserts it
//! is BIT-EXACT vs the CPU reference, identical to the direct-dispatch demo.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::staged_dir;

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
fn real_vulkan_dispatch_indirect_sources_dims_from_buffer_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vkdispind-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_dispatch_indirect");

    // Same saxpy shader as vk_compute_saxpy; the only difference is the dispatch dims come from a buffer.
    let cs_spirv = wgsl_to_spirv(
        r#"
        @group(0) @binding(0) var<storage, read>       a: array<u32>;
        @group(0) @binding(1) var<storage, read>       b: array<u32>;
        @group(0) @binding(2) var<storage, read_write> c: array<u32>;
        @compute @workgroup_size(64)
        fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
            let i = gid.x;
            c[i] = a[i] * 3u + b[i];
        }
    "#,
    );
    let cs_path = out_dir.join("vkdispind.comp.spv");
    write_spv(&cs_path, &cs_spirv);

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_dispatch_indirect.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("vkdispind");
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
        .expect("spawn vk_dispatch_indirect");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_dispatch_indirect stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("VK_DISPATCH_INDIRECT_OK"), "guest drove the indirect dispatch");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // CPU reference: A[i]=i+1, B[i]=i*7+2  =>  C[i] = 3*(i+1) + (i*7+2) = 10*i + 5.
    let expected: Vec<u32> = (0..N as u32).map(|i| 3 * (i + 1) + (i * 7 + 2)).collect();

    let cap = exec.captured();
    let want_bytes = N * 4;
    let matched = cap
        .buffers
        .values()
        .filter(|v| v.len() == want_bytes)
        .map(|v| decode_u32s(v))
        .find(|got| *got == expected);

    let candidates: Vec<Vec<u32>> = cap
        .buffers
        .values()
        .filter(|v| v.len() == want_bytes)
        .map(|v| decode_u32s(v))
        .collect();
    assert!(
        matched.is_some(),
        "indirect dispatch produced no BIT-EXACT saxpy buffer (was it dropped as a no-op?). expected head \
         {:?}, candidates heads {:?}",
        &expected[..8],
        candidates.iter().map(|c| c[..8.min(c.len())].to_vec()).collect::<Vec<_>>()
    );
}
