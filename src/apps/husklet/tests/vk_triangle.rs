//! REAL GRAPHICS #1 — a REAL C Vulkan program rasterizes a solid triangle end to end on lavapipe.
//!
//! `../../surface/hl-vulkan/tests/fixtures/vk_triangle.c` is compiled against the REAL Khronos Vulkan-Headers and driven through the REAL
//! loader + OUR ICD (`VK_ICD_FILENAMES=~/.hl/vulkan/aarch64/icd.json`). It creates a 64x64 RGBA8 color
//! image, a render pass (clear=RED), a graphics pipeline with REAL SPIR-V shaders (a `gl_VertexIndex`
//! centered triangle + a solid-GREEN fragment shader — minted here by naga, forwarded verbatim by our
//! ICD), records BeginRenderPass/Draw(3)/EndRenderPass, and copies the image to a buffer. Every call flows
//! loader → our ICD → IR → `$HL_GPU_EXEC` → the host `WgpuExecutor` on the SOFTWARE Vulkan device
//! (lavapipe / `llvmpipe`), which REALLY rasterizes the triangle via wgpu/naga (SPIR-V → WGSL → SPIR-V →
//! lavapipe). We then read the rendered target back OFF THE HOST executor and assert the raster: the
//! CENTER pixel is the triangle's GREEN and the CORNERS are the clear RED.
//!
//! WHY host-side readback: our ICD's `vkMapMemory` is write-through (no device→host readback — a real,
//! filed shim gap; see the report in this test's failure messages and `common/wgpu.rs`). So a real Vulkan
//! app cannot observe GPU output through the map path. The pixels nonetheless land on the host behind the
//! protocol ids, so the host reads them back — the furthest observable correct step, and it genuinely
//! asserts lavapipe's rasterizer output driven by our ICD's lowered IR.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const VK_HEADERS: &str = "/Users/x/vk-refs/Vulkan-Headers/include";
const LOADER: &str = "/usr/lib/aarch64-linux-gnu/libvulkan.so.1";

const W: u32 = 64;
const H: u32 = 64;

/// Mint real SPIR-V from a single-entry-point WGSL seed via naga (wgsl-in → spv-out). The executor
/// translates it straight back (spv-in → wgsl-out) and builds a real render pipeline, so the SPIR-V
/// genuinely drives lavapipe's rasterizer.
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
fn real_vulkan_triangle_rasterizes_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-vktri-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_triangle");

    // --- mint the two SPIR-V shaders (vertex + fragment) as separate single-entry modules ----------
    // A CENTERED triangle (leaves the four image corners uncovered) so we can distinguish the drawn
    // GREEN from the RED clear. `gl_VertexIndex` only — no vertex buffer.
    let vs_spirv = wgsl_to_spirv(
        r#"
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
            var p = array<vec2<f32>, 3>(
                vec2<f32>( 0.0,  0.8),
                vec2<f32>(-0.8, -0.8),
                vec2<f32>( 0.8, -0.8));
            return vec4<f32>(p[vi], 0.0, 1.0);
        }
    "#,
    );
    let fs_spirv = wgsl_to_spirv(
        r#"
        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
        }
    "#,
    );
    let vs_path = out_dir.join("triangle.vert.spv");
    let fs_path = out_dir.join("triangle.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    // --- compile the REAL Vulkan C program against real headers ------------------------------------
    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-vulkan/tests/fixtures/vk_triangle.c"
        ))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the Vulkan triangle program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // --- host executor on lavapipe, then run the real program with the loader pointed at OUR ICD ---
    let exec = WgpuExecutorServer::start("vktri");
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
        .expect("spawn vk_triangle");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_triangle stdout ---\n{stdout}\n--- vk_triangle stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "real Vulkan triangle program exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("VK_TRIANGLE_DRAW_OK"),
        "the program drove image/renderpass/graphics-pipeline/draw/copy/submit"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // --- assert the ACTUAL lavapipe raster output, read back off the host executor -----------------
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no 64x64 RGBA8 render target captured off the host executor — the ICD's graphics lowering \
             did not produce a texture lavapipe could rasterize + read back. Captured texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    });
    assert_eq!(
        px.len(),
        (W * H * 4) as usize,
        "render target is 64x64 RGBA8"
    );

    let texel = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    let green = [0u8, 255, 0, 255];
    let red = [255u8, 0, 0, 255];

    // CENTER is covered by the triangle → GREEN (the SPIR-V fragment shader ran on lavapipe).
    assert_eq!(
        texel(W / 2, H / 2),
        green,
        "center pixel should be the triangle's GREEN rasterized by the SPIR-V fragment shader on lavapipe"
    );
    // The four CORNERS are outside the centered triangle → the RED clear survives.
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
        assert_eq!(
            texel(x, y),
            red,
            "corner ({x},{y}) should be the clear RED (outside the centered triangle)"
        );
    }

    // The guest's vkCmdCopyImageToBuffer also landed the same pixels in a host buffer (the readback the
    // app tried to map). Assert its center matches too — proving the image→buffer copy lowered + executed.
    if let Some(bufpx) = cap.rgba8_buffer(W, H) {
        let i = (((H / 2) * W + (W / 2)) * 4) as usize;
        assert_eq!(
            &bufpx[i..i + 4],
            &green,
            "copy-to-buffer center pixel should match the rendered triangle"
        );
    }

    let _ = std::fs::remove_dir_all(&out_dir);
}
