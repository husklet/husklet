//! VK RENDER-CORRECTNESS DEMO #1 — vk_transform: a real C Vulkan program whose MVP (scale + translate)
//! lives in a UNIFORM BUFFER bound through a descriptor set places a full-NDC quad at an EXACT off-center
//! rectangle, rasterized on lavapipe. scale=(0.5,0.5) + offset=(0.25,0.0) maps the ±1 quad into NDC
//! x∈[-0.25,0.75], y∈[-0.5,0.5] → pixel cols [24,56) × rows [16,48) on a 64×64 target, filled GREEN over a
//! BLUE clear. Asserts the exact rect off the host executor and writes /tmp/hl-demo/vk_transform.png.

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

#[test]
fn real_vulkan_ubo_mvp_places_quad_at_exact_rect_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vktransform-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_transform");

    // Full-NDC quad (6 verts, no vertex buffer); the vertex shader reads scale+offset from the std140 UBO.
    let vs_spirv = wgsl_to_spirv(
        r#"
        struct Xform { scale: vec4<f32>, offset: vec4<f32> };
        @group(0) @binding(0) var<uniform> u: Xform;
        struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
            var P = array<vec2<f32>, 6>(
                vec2<f32>(-1.0,  1.0), vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0,  1.0),
                vec2<f32>( 1.0,  1.0), vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0, -1.0));
            var o: VOut;
            o.pos = vec4<f32>(P[vi] * u.scale.xy + u.offset.xy, 0.0, 1.0);
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
    let vs_path = out_dir.join("xform.vert.spv");
    let fs_path = out_dir.join("xform.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_transform.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("vktransform");
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
        .expect("spawn vk_transform");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_transform stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("VK_TRANSFORM_OK"), "guest drove the UBO-transform draw");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read + PNG. Render target is Rgba8Unorm, so px is already RGBA (R,G,B,A) — no swizzle.
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    write_png(&demo_png_dir().join("vk_transform.png"), W, H, px);

    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (R,G,B,A)
    };
    let is_green = |x, y| {
        let (r, g, b, a) = at(x, y);
        near(r, 0) && near(g, 255) && near(b, 0) && a == 255
    };
    let is_blue = |x, y| {
        let (r, g, b, a) = at(x, y);
        near(r, 0) && near(g, 0) && near(b, 255) && a == 255
    };

    assert!(is_green(40, 32), "rect interior must be GREEN, got {:?}", at(40, 32));
    assert!(is_blue(12, 32), "left of the rect must be the BLUE clear, got {:?}", at(12, 32));
    assert!(is_blue(58, 32), "right of the rect must be the BLUE clear, got {:?}", at(58, 32));
    assert!(is_blue(40, 6), "above the rect must be the BLUE clear, got {:?}", at(40, 6));
    assert!(is_blue(40, 58), "below the rect must be the BLUE clear, got {:?}", at(40, 58));

    // The transformed GREEN fill must cover ~the rect's area (32×32 = 1024 px of 4096) — a real
    // scale+translate, not a collapsed (nothing) or identity (full-frame) matrix.
    let green = px
        .chunks_exact(4)
        .filter(|t| near(t[0], 0) && near(t[1], 255) && near(t[2], 0) && t[3] == 255)
        .count();
    assert!(
        (700..1400).contains(&green),
        "the transformed GREEN quad must cover ~1024 px (the exact rect), got {green}"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
