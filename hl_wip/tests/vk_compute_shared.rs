//! VK COMPUTE DEMO — vk_compute_shared: a real C Vulkan program dispatches a compute shader that uses
//! WORKGROUP SHARED MEMORY (`var<workgroup>`) + `workgroupBarrier()` to tree-reduce each workgroup's 64
//! lane values, lane 0 writing the workgroup sum to `output[workgroup_id]`. 256 inputs (input[i]=i+1),
//! `@workgroup_size(64)`, 4 workgroups → 4 sums. The whole thing flows through the REAL
//! `vkCreateComputePipelines` + `vkCmdDispatch` shim lowering (loader → OUR ICD → IR → WgpuExecutor/
//! lavapipe); the host reads `output` back off the executor and asserts each of the 4 sums is BIT-EXACT vs
//! the CPU reference — proving shared memory + barriers work end to end.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::staged_dir;

const VK_HEADERS: &str = "/Users/x/vk-refs/Vulkan-Headers/include";
const LOADER: &str = "/usr/lib/aarch64-linux-gnu/libvulkan.so.1";

const GROUPS: usize = 4;

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
fn real_vulkan_compute_shared_memory_reduction_is_bit_exact_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vkshared-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_compute_shared");

    // Per-workgroup tree reduction in shared memory: stage 64 lanes, barrier, halve-stride sum, barrier.
    let cs_spirv = wgsl_to_spirv(
        r#"
        @group(0) @binding(0) var<storage, read>       input:  array<u32>;
        @group(0) @binding(1) var<storage, read_write> output: array<u32>;
        var<workgroup> scratch: array<u32, 64>;
        @compute @workgroup_size(64)
        fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>,
                   @builtin(local_invocation_id)  lid: vec3<u32>,
                   @builtin(workgroup_id)         wid: vec3<u32>) {
            scratch[lid.x] = input[gid.x];
            workgroupBarrier();
            var stride: u32 = 32u;
            loop {
                if (stride == 0u) { break; }
                if (lid.x < stride) {
                    scratch[lid.x] = scratch[lid.x] + scratch[lid.x + stride];
                }
                workgroupBarrier();
                stride = stride / 2u;
            }
            if (lid.x == 0u) {
                output[wid.x] = scratch[0];
            }
        }
    "#,
    );
    let cs_path = out_dir.join("vkshared.comp.spv");
    write_spv(&cs_path, &cs_spirv);

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_compute_shared.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("vkshared");
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
        .expect("spawn vk_compute_shared");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_compute_shared stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("VK_COMPUTE_SHARED_OK"), "guest drove the shared-memory reduction");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // CPU reference: input[i]=i+1, so output[w] = sum_{i in [64w, 64w+64)} (i+1).
    let expected: Vec<u32> = (0..GROUPS as u32)
        .map(|w| (0..64u32).map(|j| 64 * w + j + 1).sum())
        .collect();

    // The output buffer is the unique 4-u32 (16-byte) buffer captured off the executor.
    let cap = exec.captured();
    let want_bytes = GROUPS * 4;
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
        "shared-memory reduction produced no BIT-EXACT sum buffer. expected {:?}, candidates {:?}",
        expected, candidates
    );
}
