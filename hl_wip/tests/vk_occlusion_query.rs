//! VK ADVANCED-COMMAND DEMO — vk_occlusion_query: a real C Vulkan program wraps two draws of the same
//! full-screen GREEN quad in a VK_QUERY_TYPE_OCCLUSION pool with a dynamic scissor. Query 0 (scissor =
//! LEFT half) rasterizes 32*64 = 2048 samples and reports 2048; query 1 (scissor = EMPTY) is fully
//! scissored and reports 0. vkGetQueryPoolResults reads both counts back and the guest self-checks them.
//! This guards the shim's occlusion model — before the fix vkCmdEndQuery recorded a constant 0 regardless
//! of coverage (a false result). The Rust test re-parses the printed counts and asserts the exact
//! left-green / right-black raster, then writes /tmp/hl-demo/vk_occlusion_query.png.

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

/// Pull the `u64` after `key=` out of a `KEY0=.. KEY1=..` line of guest stdout.
fn parse_kv(stdout: &str, key: &str) -> Option<u64> {
    stdout
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(key).and_then(|v| v.parse::<u64>().ok()))
}

#[test]
fn real_vulkan_occlusion_query_reflects_drawn_coverage_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vkocclusion-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_occlusion_query");

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
    let vs_path = out_dir.join("vkocclusion.vert.spv");
    let fs_path = out_dir.join("vkocclusion.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_occlusion_query.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("vkocclusion");
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
        .expect("spawn vk_occlusion_query");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_occlusion_query stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("VK_OCCLUSION_OK"), "guest self-checked the occlusion counts");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Re-parse the query counts the guest read back: a visible draw counts its coverage, an occluded one 0.
    let q0 = parse_kv(&stdout, "Q0=").expect("Q0 count in stdout");
    let q1 = parse_kv(&stdout, "Q1=").expect("Q1 count in stdout");
    assert_eq!(q0, 2048, "visible LEFT-half draw must count 32*64 = 2048 samples");
    assert_eq!(q1, 0, "fully-scissored draw must count 0 samples (occluded)");

    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    write_png(&demo_png_dir().join("vk_occlusion_query.png"), W, H, px);

    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (R,G,B,A)
    };
    let is_green = |x, y| {
        let (r, g, b, a) = at(x, y);
        near(r, 0) && near(g, 255) && near(b, 0) && a == 255
    };
    let is_black = |x, y| {
        let (r, g, b, a) = at(x, y);
        near(r, 0) && near(g, 0) && near(b, 0) && a == 255
    };
    // The visible query's LEFT-half draw rendered green; the occluded query added nothing (right stays black).
    assert!(is_green(16, 32), "Q0 LEFT-half draw must be GREEN, got {:?}", at(16, 32));
    assert!(is_green(2, 2), "Q0 LEFT-half draw must reach the corner, got {:?}", at(2, 2));
    assert!(is_black(48, 32), "the occluded Q1 draw must leave the RIGHT half the BLACK clear, got {:?}", at(48, 32));
    assert!(is_black(60, 60), "the RIGHT half stays the BLACK clear, got {:?}", at(60, 60));
    // The GREEN must cover ~the left half (32*64 = 2048 px) — exactly Q0's reported sample count.
    let green = px.chunks_exact(4).filter(|t| near(t[0], 0) && near(t[1], 255) && near(t[2], 0) && t[3] == 255).count();
    assert_eq!(green as u64, q0, "the GREEN pixel count must equal Q0's occlusion sample count");
    let _ = std::fs::remove_dir_all(&out_dir);
}
