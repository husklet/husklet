//! VK ADVANCED-COMMAND DEMO — vk_timestamp: a real C Vulkan program writes a VK_QUERY_TYPE_TIMESTAMP pair
//! straddling a full-screen GREEN quad draw (`vkCmdWriteTimestamp` TOP_OF_PIPE before, BOTTOM_OF_PIPE
//! after), reads both serials back with vkGetQueryPoolResults, and self-checks that the end timestamp is
//! >= (here strictly >) the begin one — the one guarantee an app may rely on across two timestamps in
//! submission order. The Rust test re-parses the serials, asserts monotonicity, and asserts the drawn GREEN
//! frame, writing /tmp/hl-demo/vk_timestamp.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{demo_png_dir, staged_dir, write_png};

const VK_HEADERS: &str = "/Users/x/vk-refs/Vulkan-Headers/include";
const LOADER: &str = "/usr/lib/aarch64-linux-gnu/libvulkan.so.1";

const W: u32 = 64;
const H: u32 = 64;

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

/// Pull the `u64` after `key` (e.g. `"T0="`) out of a whitespace-delimited stdout token.
fn parse_kv(stdout: &str, key: &str) -> Option<u64> {
    stdout
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(key).and_then(|v| v.parse::<u64>().ok()))
}

#[test]
fn real_vulkan_timestamp_pair_is_monotonic_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vktimestamp-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_timestamp");

    let vs_spirv = wgsl_to_spirv(
        r#"
        struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
            var P = array<vec2<f32>, 6>(
                vec2<f32>(-1.0,  1.0), vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0,  1.0),
                vec2<f32>( 1.0,  1.0), vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0, -1.0));
            var o: VOut;
            o.pos = vec4<f32>(P[vi], 0.0, 1.0);
            o.color = vec4<f32>(0.0, 1.0, 0.0, 1.0);
            return o;
        }
    "#,
    );
    let fs_spirv = wgsl_to_spirv(
        r#"
        @fragment
        fn fs_main(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> { return color; }
    "#,
    );
    let vs_path = out_dir.join("vktimestamp.vert.spv");
    let fs_path = out_dir.join("vktimestamp.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-vulkan/tests/fixtures/vk_timestamp.c"
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

    let exec = WgpuExecutorServer::start("vktimestamp");
    let adapter = exec.adapter_name();
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "must rasterize on the software Vulkan device, got {adapter:?}"
    );

    let run = Command::new(&bin)
        .env("VK_ICD_FILENAMES", &icd)
        .env("VK_DRIVER_FILES", &icd)
        .env("VK_LOADER_LAYERS_DISABLE", "~all~")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_VK_VS_SPV", &vs_path)
        .env("HL_VK_FS_SPV", &fs_path)
        .env("VK_LOADER_DEBUG", "error,warn")
        .output()
        .expect("spawn vk_timestamp");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_timestamp stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("VK_TIMESTAMP_OK"),
        "guest self-checked the timestamp pair"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // Re-parse the two serials the guest read back — the end must be >= the begin (monotonic).
    let t0 = parse_kv(&stdout, "T0=").expect("T0 serial in stdout");
    let t1 = parse_kv(&stdout, "T1=").expect("T1 serial in stdout");
    assert!(
        t1 >= t0,
        "timestamp pair must be monotonic (T0={t0}, T1={t1})"
    );
    assert!(
        t1 > t0,
        "the host-serial model strictly increases (T0={t0}, T1={t1})"
    );

    let cap = exec.captured();
    let px = cap
        .rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor");
    write_png(&demo_png_dir().join("vk_timestamp.png"), W, H, px);

    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (R,G,B,A)
    };
    let is_green = |x, y| {
        let (r, g, b, a) = at(x, y);
        near(r, 0) && near(g, 255) && near(b, 0) && a == 255
    };
    // The draw between the two timestamps produced the full-screen GREEN frame.
    assert!(
        is_green(32, 32),
        "center must be GREEN, got {:?}",
        at(32, 32)
    );
    assert!(is_green(2, 2), "corner must be GREEN, got {:?}", at(2, 2));
    assert!(
        is_green(61, 61),
        "far corner must be GREEN, got {:?}",
        at(61, 61)
    );
    let green = px
        .chunks_exact(4)
        .filter(|t| near(t[0], 0) && near(t[1], 255) && near(t[2], 0) && t[3] == 255)
        .count();
    assert!(
        green > (W * H * 9 / 10) as usize,
        "the full-screen quad must fill the frame, got {green} green px"
    );
    let _ = std::fs::remove_dir_all(&out_dir);
}
