//! VK SYNC DEMO — vk_event: a real C Vulkan program records `vkCmdSetEvent` + `vkCmdWaitEvents` between a
//! producer compute dispatch (`P[i] = A[i]*2`) and a dependent consumer dispatch (`C[i] = P[i] + 7`, which
//! reads the producer's P) in ONE command buffer. After the (synchronous) submit the guest proves the
//! recorded device set resolved (`vkGetEventStatus == VK_EVENT_SET`), that host `vkResetEvent` /
//! `vkSetEvent` flip the status, and that a bad `VkEvent` returns a real error. The whole thing flows
//! through the REAL event shim lowering (loader → OUR ICD → IR → WgpuExecutor/lavapipe); the host reads
//! BOTH P and C back off the executor and asserts each is bit-exact, proving the event ordered the
//! dependent read after the producer's write.

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
fn real_vulkan_event_orders_dependent_read_after_producer_bit_exact() {
    let vk_dir = staged_dir("vulkan");
    let icd = vk_dir.join("icd.json");
    assert!(icd.exists(), "staged ICD manifest missing at {icd:?}");
    assert!(
        vk_dir.join("libvk_hl.so").exists(),
        "staged libvk_hl.so missing at {vk_dir:?}"
    );
    if !std::path::Path::new(LOADER).exists() {
        eprintln!("SKIP: real Vulkan loader {LOADER} not present");
        return;
    }
    if !std::path::Path::new(VK_HEADERS).exists() {
        eprintln!("SKIP: Vulkan-Headers not found at {VK_HEADERS}");
        return;
    }

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vkevent-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_event");

    // Producer: P[i] = A[i]*2 (bindings 0 read = A, 1 write = P).
    let prod_spirv = wgsl_to_spirv(
        r#"
        @group(0) @binding(0) var<storage, read>       a: array<u32>;
        @group(0) @binding(1) var<storage, read_write> p: array<u32>;
        @compute @workgroup_size(64)
        fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
            let i = gid.x;
            p[i] = a[i] * 2u;
        }
    "#,
    );
    // Consumer: C[i] = P[i] + 7 (bindings 0 read = P, 1 write = C).
    let cons_spirv = wgsl_to_spirv(
        r#"
        @group(0) @binding(0) var<storage, read>       p: array<u32>;
        @group(0) @binding(1) var<storage, read_write> c: array<u32>;
        @compute @workgroup_size(64)
        fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
            let i = gid.x;
            c[i] = p[i] + 7u;
        }
    "#,
    );
    let prod_path = out_dir.join("vkevent.prod.spv");
    let cons_path = out_dir.join("vkevent.cons.spv");
    write_spv(&prod_path, &prod_spirv);
    write_spv(&cons_path, &cons_spirv);

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-vulkan/tests/fixtures/vk_event.c"
        ))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exec = WgpuExecutorServer::start("vkevent");
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
        .env("HL_VK_PROD_SPV", &prod_path)
        .env("HL_VK_CONS_SPV", &cons_path)
        .env("VK_LOADER_DEBUG", "error,warn")
        .output()
        .expect("spawn vk_event");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_event stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    // The guest itself asserts the recorded set resolved (VK_EVENT_SET), the reset/set flips, and that a
    // bad VkEvent errors — a non-zero exit means one of those checks failed.
    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("VK_EVENT_OK"),
        "guest drove the event-ordered producer→consumer"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // A[i]=i+1 => producer P[i] = 2*(i+1) = 2i+2; consumer C[i] = P[i] + 7 = 2i+9.
    let expect_p: Vec<u32> = (0..N as u32).map(|i| 2 * (i + 1)).collect();
    let expect_c: Vec<u32> = (0..N as u32).map(|i| 2 * (i + 1) + 7).collect();

    let cap = exec.captured();
    let want_bytes = N * 4;
    let decoded: Vec<Vec<u32>> = cap
        .buffers
        .values()
        .filter(|v| v.len() == want_bytes)
        .map(|v| decode_u32s(v))
        .collect();

    let saw_p = decoded.iter().any(|g| *g == expect_p);
    let saw_c = decoded.iter().any(|g| *g == expect_c);
    assert!(
        saw_p,
        "producer output P not found bit-exact. expected head {:?}, candidates {:?}",
        &expect_p[..8],
        decoded
            .iter()
            .map(|c| c[..8.min(c.len())].to_vec())
            .collect::<Vec<_>>()
    );
    assert!(
        saw_c,
        "consumer output C (= producer's P + 7) not found bit-exact — the event did not order the \
         dependent read after the producer's write. expected head {:?}, candidates {:?}",
        &expect_c[..8],
        decoded
            .iter()
            .map(|c| c[..8.min(c.len())].to_vec())
            .collect::<Vec<_>>()
    );
}
