//! REAL GRAPHICS #2 — a REAL C Vulkan program rasterizes a MULTI-TRIANGLE, per-vertex-COLORED quad on
//! lavapipe, and we assert the interpolated raster.
//!
//! This escalates `vk_triangle.rs` (one flat solid triangle) toward what mission escalation #1 targets — a
//! multi-triangle mesh with full coverage and per-corner interpolated vertex colors — using the parts that
//! are supported END TO END today: a NON-indexed `vkCmdDraw(6)` (two triangles) with `@builtin(vertex_index)`
//! positions + a per-vertex `@location(0)` color the rasterizer interpolates. `csrc/vk_quad.c` is compiled
//! against the REAL Vulkan-Headers and driven through the REAL loader + OUR ICD; every call flows
//! loader → ICD → IR → `$HL_GPU_EXEC` → the host `WgpuExecutor` on lavapipe, which REALLY rasterizes and
//! interpolates. We read the render target back OFF the host executor and assert:
//!   * each of the four corners is ~its assigned vertex color (proves the 2-triangle mesh covered the whole
//!     image AND that vertex-color interpolation ran on lavapipe), and
//!   * NO pixel is the BLACK clear (proves the quad fully covered the framebuffer — distinct clear color).
//!
//! WHY NOT `vkCmdDrawIndexed` (escalation #1 as literally specified): our Vulkan driver lowers
//! `vkCmdBindIndexBuffer`/`vkCmdDrawIndexed` correctly (`hl_wip-vulkan/src/service/record.rs`), but the wgpu
//! executor's render-pass replay only issues `Enc::Draw` — `Enc::DrawIndexed` / `Enc::SetIndexBuffer` /
//! `Enc::SetVertexBuffer` fall through unhandled in `hl_wip-gpu-wgpu/src/submit.rs::run_render_pass`, so an
//! indexed draw would rasterize NOTHING. (Vertex-buffer layouts are also rejected outright at
//! `hl_wip-gpu-wgpu/src/pipeline.rs::create_render_pipeline`.) See the mission report for the precise gap.
//! A non-indexed multi-triangle draw with interpolation is the real, passing variant of that escalation.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::staged_dir;

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
fn real_vulkan_colored_quad_interpolates_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-vkquad-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_quad");

    // --- two-triangle full-screen quad (6 verts via gl_VertexIndex, no vertex buffer) ------------
    // Corner colors: TL red, TR green, BL blue, BR yellow. wgpu clip space: y=+1 is the TOP row.
    // Split TL,BL,TR / TR,BL,BR so each image corner is dominated by its own vertex color.
    let vs_spirv = wgsl_to_spirv(
        r#"
        struct VOut {
            @builtin(position) pos: vec4<f32>,
            @location(0) color: vec4<f32>,
        };
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
            var P = array<vec2<f32>, 6>(
                vec2<f32>(-1.0,  1.0),   // 0 TL
                vec2<f32>(-1.0, -1.0),   // 1 BL
                vec2<f32>( 1.0,  1.0),   // 2 TR
                vec2<f32>( 1.0,  1.0),   // 3 TR
                vec2<f32>(-1.0, -1.0),   // 4 BL
                vec2<f32>( 1.0, -1.0));  // 5 BR
            var C = array<vec4<f32>, 6>(
                vec4<f32>(1.0, 0.0, 0.0, 1.0),   // TL red
                vec4<f32>(0.0, 0.0, 1.0, 1.0),   // BL blue
                vec4<f32>(0.0, 1.0, 0.0, 1.0),   // TR green
                vec4<f32>(0.0, 1.0, 0.0, 1.0),   // TR green
                vec4<f32>(0.0, 0.0, 1.0, 1.0),   // BL blue
                vec4<f32>(1.0, 1.0, 0.0, 1.0));  // BR yellow
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
        fn fs_main(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> {
            return color;
        }
    "#,
    );
    let vs_path = out_dir.join("quad.vert.spv");
    let fs_path = out_dir.join("quad.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    // --- compile the REAL Vulkan C program -----------------------------------------------------
    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/vk_quad.c"))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the Vulkan quad program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // --- host executor on lavapipe, then run the real program ----------------------------------
    let exec = WgpuExecutorServer::start("vkquad");
    eprintln!("host wgpu adapter: {}", exec.adapter_name());

    let run = Command::new(&bin)
        .env("VK_ICD_FILENAMES", &icd)
        .env("VK_DRIVER_FILES", &icd)
        .env("VK_LOADER_LAYERS_DISABLE", "~all~")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_VK_VS_SPV", &vs_path)
        .env("HL_VK_FS_SPV", &fs_path)
        .env("VK_LOADER_DEBUG", "error,warn")
        .output()
        .expect("spawn vk_quad");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_quad stdout ---\n{stdout}\n--- vk_quad stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "real Vulkan quad program exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("VK_QUAD_DRAW_OK"),
        "the program drove image/renderpass/graphics-pipeline/draw(6)/copy/submit"
    );
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // --- assert the ACTUAL lavapipe interpolated raster ----------------------------------------
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no 64x64 RGBA8 render target captured off the host executor. Captured texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    });
    assert_eq!(px.len(), (W * H * 4) as usize, "render target is 64x64 RGBA8");

    let texel = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };

    // Sample a few pixels IN from each corner so the interpolation is clearly dominated by that
    // corner's vertex color (the exact corner texel is pulled slightly toward its diagonal neighbor).
    let tl = texel(3, 3);
    let tr = texel(W - 4, 3);
    let bl = texel(3, H - 4);
    let br = texel(W - 4, H - 4);
    let center = texel(W / 2, H / 2);
    eprintln!("corners: TL={tl:?} TR={tr:?} BL={bl:?} BR={br:?} center={center:?}");

    // Each corner must be strongly dominated by its assigned channel(s) — proving the 2-triangle mesh
    // covered the corner AND that per-vertex color interpolation ran on lavapipe.
    let dominant_red = |c: [u8; 4]| c[0] > 170 && c[1] < 90 && c[2] < 90;
    let dominant_green = |c: [u8; 4]| c[1] > 170 && c[0] < 90 && c[2] < 90;
    let dominant_blue = |c: [u8; 4]| c[2] > 170 && c[0] < 90 && c[1] < 90;
    let dominant_yellow = |c: [u8; 4]| c[0] > 150 && c[1] > 150 && c[2] < 90;

    assert!(dominant_red(tl), "top-left corner should be ~RED (its vertex color), got {tl:?}");
    assert!(dominant_green(tr), "top-right corner should be ~GREEN, got {tr:?}");
    assert!(dominant_blue(bl), "bottom-left corner should be ~BLUE, got {bl:?}");
    assert!(dominant_yellow(br), "bottom-right corner should be ~YELLOW, got {br:?}");

    // The center sits on the TR(green)–BL(blue) split diagonal, so it interpolates to a green+blue
    // BLEND (teal) — a genuine mix that is neither any pure corner color nor the clear, confirming
    // smooth per-vertex interpolation ran across the two triangles.
    assert!(
        center[1] > 40 && center[2] > 40 && center[0] < 90 && center != [0, 0, 0, 255],
        "center should be an interpolated green+blue blend, got {center:?}"
    );

    // FULL COVERAGE: with a full-screen quad the BLACK clear must survive NOWHERE. Any near-black pixel
    // would mean an uncovered fragment (the interpolated corner colors are all bright).
    let mut black = 0usize;
    for y in 0..H {
        for x in 0..W {
            let c = texel(x, y);
            if c[0] < 12 && c[1] < 12 && c[2] < 12 {
                black += 1;
            }
        }
    }
    assert_eq!(black, 0, "the quad must fully cover the framebuffer — found {black} clear (black) pixels");

    // The copy-to-buffer landed the same raster — assert its top-left matches the texture readback.
    if let Some(bufpx) = cap.rgba8_buffer(W, H) {
        let i = ((3 * W + 3) * 4) as usize;
        assert!(
            dominant_red([bufpx[i], bufpx[i + 1], bufpx[i + 2], bufpx[i + 3]]),
            "copy-to-buffer top-left should match the rendered quad's RED corner"
        );
    }

    let _ = std::fs::remove_dir_all(&out_dir);
}
