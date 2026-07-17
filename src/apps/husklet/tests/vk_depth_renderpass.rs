//! VK RENDER-CORRECTNESS DEMO — vk_depth_renderpass: a real C Vulkan program proves the depth test occludes
//! correctly on the CLASSIC VkRenderPass + VkFramebuffer path (NOT dynamic rendering) on lavapipe. An
//! explicit VkRenderPass declares a color attachment (0) + a D32_SFLOAT depth/stencil attachment (1); a
//! VkFramebuffer binds a color view + a depth image view; vkCmdBeginRenderPass threads the depth buffer.
//! A NEAR GREEN quad over the LEFT half (z=0.3) is drawn FIRST, then a FAR RED full-screen quad (z=0.7);
//! with depthCompareOp=LESS the far quad fails the depth test over the near geometry, so the left half
//! stays GREEN and only the right becomes RED.
//!
//! This is the regression guard for the shim threading a CLASSIC render pass's depth attachment into the IR.
//! Before the fix, record.rs cmd_begin_render_pass hardcoded `depth: None` and RenderPassRec tracked no
//! depth attachment, so a classic depth-tested pipeline got NO depth target: the near quad could not occlude
//! the far one, and (because the pipeline's depth-stencil state IS threaded) the executor rejects the pass —
//! "the RenderPass uses a texture with format None but the RenderPipeline uses Some(Depth32Float)" — so the
//! frame never resolves the two-quad occlusion at all. Verified by forcing `depth: None` back into the
//! classic path: this exact test fails at the green-half assertion. Asserts the depth-resolved raster off the
//! host executor and writes /tmp/hl-demo/vk_depth_renderpass.png.

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
fn real_vulkan_classic_renderpass_depth_occludes_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vkdepthrp-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_depth_renderpass");

    // 12 verts: 0..5 = NEAR green LEFT-half quad (z=0.3); 6..11 = FAR red FULL quad (z=0.7).
    let vs_spirv = wgsl_to_spirv(
        r#"
        struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
            var P = array<vec3<f32>, 12>(
                vec3<f32>(-1.0,  1.0, 0.3), vec3<f32>(-1.0, -1.0, 0.3), vec3<f32>( 0.0,  1.0, 0.3),
                vec3<f32>( 0.0,  1.0, 0.3), vec3<f32>(-1.0, -1.0, 0.3), vec3<f32>( 0.0, -1.0, 0.3),
                vec3<f32>(-1.0,  1.0, 0.7), vec3<f32>(-1.0, -1.0, 0.7), vec3<f32>( 1.0,  1.0, 0.7),
                vec3<f32>( 1.0,  1.0, 0.7), vec3<f32>(-1.0, -1.0, 0.7), vec3<f32>( 1.0, -1.0, 0.7));
            var C = array<vec4<f32>, 12>(
                vec4<f32>(0.0,1.0,0.0,1.0), vec4<f32>(0.0,1.0,0.0,1.0), vec4<f32>(0.0,1.0,0.0,1.0),
                vec4<f32>(0.0,1.0,0.0,1.0), vec4<f32>(0.0,1.0,0.0,1.0), vec4<f32>(0.0,1.0,0.0,1.0),
                vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0),
                vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0), vec4<f32>(1.0,0.0,0.0,1.0));
            var o: VOut;
            o.pos = vec4<f32>(P[vi], 1.0);
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
    let vs_path = out_dir.join("depth_rp.vert.spv");
    let fs_path = out_dir.join("depth_rp.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-vulkan/tests/fixtures/vk_depth_renderpass.c"
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

    let exec = WgpuExecutorServer::start("vkdepthrp");
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
        .expect("spawn vk_depth_renderpass");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_depth_renderpass stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("VK_DEPTH_RP_OK"),
        "guest drove the depth-tested classic-render-pass draws"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // Host read + PNG. Rgba8Unorm target → px is RGBA.
    let cap = exec.captured();
    let px = cap
        .rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor");
    write_png(&demo_png_dir().join("vk_depth_renderpass.png"), W, H, px);

    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (R,G,B,A)
    };

    // Flat-shaded solid quads → the interior pixels are EXACTLY the vertex color (no interpolation), so
    // assert exact 8-bit values well inside each half (x=16 and x=48 straddle the x=32 quad boundary).
    let left = at(16, 32); // left: NEAR GREEN wins (far quad occluded by the depth buffer)
    assert_eq!(
        left,
        (0, 255, 0, 255),
        "left half must be EXACTLY the NEAR GREEN quad (classic-render-pass depth occlusion), got {left:?}"
    );
    let right = at(48, 32); // right: FAR RED only
    assert_eq!(
        right,
        (255, 0, 0, 255),
        "right half must be EXACTLY the FAR RED quad, got {right:?}"
    );

    // Both colors present in meaningful amounts — a genuine two-quad depth-resolved scene. A DROPPED
    // classic-path depth buffer (the pre-fix bug) gives the depth-tested pipeline no depth target, so the
    // near quad cannot occlude the far one and the executor rejects the depth-less pass outright — either
    // way this two-color split can only exist BECAUSE the classic render pass threaded a real depth buffer.
    let green = px
        .chunks_exact(4)
        .filter(|t| near(t[0], 0) && near(t[1], 255) && near(t[2], 0))
        .count();
    let red = px
        .chunks_exact(4)
        .filter(|t| near(t[0], 255) && near(t[1], 0) && near(t[2], 0))
        .count();
    assert!(
        green > (W * H / 8) as usize && red > (W * H / 8) as usize,
        "both NEAR ({green}) and FAR ({red}) regions must survive the classic-render-pass depth test"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
