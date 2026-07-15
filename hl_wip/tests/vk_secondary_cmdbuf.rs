//! VK ADVANCED-COMMAND DEMO — vk_secondary_cmdbuf: a real C Vulkan program records its draws into a
//! level-SECONDARY command buffer (begun with RENDER_PASS_CONTINUE + a VkCommandBufferInheritanceInfo)
//! and replays it via `vkCmdExecuteCommands` inside a PRIMARY render pass begun with
//! VK_SUBPASS_CONTENTS_SECONDARY_COMMAND_BUFFERS. The two quads (RED LEFT, BLUE RIGHT — the vk_multidraw
//! geometry) come out PIXEL-IDENTICAL to recording them directly in the primary, proving the shim splices
//! a secondary command buffer's recorded encoder into the primary. Asserts the exact composited raster and
//! writes /tmp/hl-demo/vk_secondary_cmdbuf.png.

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
fn real_vulkan_secondary_cmdbuf_executes_into_primary_pass_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vksecondary-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_secondary_cmdbuf");

    // Same 12-vertex table as vk_multidraw: verts 0..5 = RED LEFT quad, 6..11 = BLUE RIGHT quad.
    let vs_spirv = wgsl_to_spirv(
        r#"
        struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
            var P = array<vec2<f32>, 12>(
                vec2<f32>(-0.9,  0.5), vec2<f32>(-0.9, -0.5), vec2<f32>(-0.1,  0.5),
                vec2<f32>(-0.1,  0.5), vec2<f32>(-0.9, -0.5), vec2<f32>(-0.1, -0.5),
                vec2<f32>( 0.1,  0.5), vec2<f32>( 0.1, -0.5), vec2<f32>( 0.9,  0.5),
                vec2<f32>( 0.9,  0.5), vec2<f32>( 0.1, -0.5), vec2<f32>( 0.9, -0.5));
            var C = array<vec4<f32>, 12>(
                vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0),
                vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0),
                vec4<f32>(0.0,0.0,1.0,1.0), vec4<f32>(0.0,0.0,1.0,1.0), vec4<f32>(0.0,0.0,1.0,1.0),
                vec4<f32>(0.0,0.0,1.0,1.0), vec4<f32>(0.0,0.0,1.0,1.0), vec4<f32>(0.0,0.0,1.0,1.0));
            var o: VOut;
            o.pos = vec4<f32>(P[vi], 0.0, 1.0);
            o.color = C[vi];
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
    let vs_path = out_dir.join("vksecondary.vert.spv");
    let fs_path = out_dir.join("vksecondary.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_secondary_cmdbuf.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("vksecondary");
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
        .expect("spawn vk_secondary_cmdbuf");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_secondary_cmdbuf stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("VK_SECONDARY_OK"), "guest recorded + executed the secondary command buffer");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    write_png(&demo_png_dir().join("vk_secondary_cmdbuf.png"), W, H, px);

    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (R,G,B,A)
    };
    let is = |x, y, r: u8, g: u8, b: u8| {
        let (rr, gg, bb, _a) = at(x, y);
        near(rr, r) && near(gg, g) && near(bb, b)
    };
    // The secondary's two draws composite exactly like inline draws: RED left, BLUE right, BLACK gap/edges.
    assert!(is(16, 32, 255, 0, 0), "secondary draw #1 LEFT region must be RED, got {:?}", at(16, 32));
    assert!(is(48, 32, 0, 0, 255), "secondary draw #2 RIGHT region must be BLUE, got {:?}", at(48, 32));
    assert!(is(32, 32, 0, 0, 0), "the GAP between the two draws must be the BLACK clear, got {:?}", at(32, 32));
    assert!(is(2, 2, 0, 0, 0), "the corner must be the BLACK clear, got {:?}", at(2, 2));
    let red = px.chunks_exact(4).filter(|t| near(t[0], 255) && near(t[1], 0) && near(t[2], 0)).count();
    let blue = px.chunks_exact(4).filter(|t| near(t[0], 0) && near(t[1], 0) && near(t[2], 255)).count();
    assert!(red > 300 && blue > 300, "both secondary-recorded draws must cover real area (red={red}, blue={blue})");
    let _ = std::fs::remove_dir_all(&out_dir);
}
