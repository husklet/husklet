//! REAL GRAPHICS #3 — a REAL C Vulkan program renders a triangle whose position is driven by a UNIFORM
//! BUFFER bound through a DESCRIPTOR SET, and we assert the geometry actually MOVED.
//!
//! This is mission escalation #2 (descriptors + a UBO-driven transform), which IS supported end to end:
//! `../../surface/hl-vulkan/tests/fixtures/vk_ubo.c` builds a descriptor-set layout (one `UNIFORM_BUFFER` binding at VERTEX stage), a
//! pipeline layout, a descriptor pool + set, a host-visible UNIFORM buffer it fills with the translation
//! `(+0.9, 0.0)` via map/write, points the set's binding 0 at that buffer with `vkUpdateDescriptorSets`,
//! and binds it with `vkCmdBindDescriptorSets` before the draw. The vertex shader reads the uniform at
//! `@group(0) @binding(0)` and adds it to a small triangle centered at the origin, pushing it to the RIGHT.
//!
//! Every call flows loader → OUR ICD → IR → `$HL_GPU_EXEC` → the host `WgpuExecutor` on lavapipe: the
//! descriptor set lowers to `Cmd::CreateBindGroup` and is built against the render pipeline's auto layout,
//! and the UBO write lands in the wgpu buffer. We read the render target back OFF the host executor and
//! assert the transform TOOK EFFECT: GREEN appears in the RIGHT half and NOT the left, and the center
//! (where the un-translated triangle would sit) is back to the RED clear. A pass proves the UBO VALUE
//! genuinely reached the shader through the descriptor path — if the descriptor never bound, wgpu's auto
//! layout would fail the draw and nothing would render; if the UBO read `(0,0)` the triangle would stay
//! centered (the opposite of what we assert).

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

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
fn real_vulkan_ubo_transform_moves_geometry_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-vkubo-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_ubo");

    // --- vertex shader: a small centered triangle translated by a uniform vec2 (@group0 @binding0) ---
    // The uniform is padded to a vec4 to match the C side's std140 write.
    let vs_spirv = wgsl_to_spirv(
        r#"
        struct Xform { offset: vec4<f32> };
        @group(0) @binding(0) var<uniform> u: Xform;
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
            var p = array<vec2<f32>, 3>(
                vec2<f32>( 0.0,  0.4),
                vec2<f32>(-0.4, -0.4),
                vec2<f32>( 0.4, -0.4));
            return vec4<f32>(p[vi] + u.offset.xy, 0.0, 1.0);
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
    let vs_path = out_dir.join("ubo.vert.spv");
    let fs_path = out_dir.join("ubo.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    // --- compile the REAL Vulkan C program -----------------------------------------------------
    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-vulkan/tests/fixtures/vk_ubo.c"
        ))
        .args(["-I", VK_HEADERS])
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the Vulkan UBO program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // --- host executor on lavapipe, then run the real program ----------------------------------
    let exec = WgpuExecutorServer::start("vkubo");
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
        .expect("spawn vk_ubo");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_ubo stdout ---\n{stdout}\n--- vk_ubo stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "real Vulkan UBO program exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("VK_UBO_DRAW_OK"),
        "the program drove descriptor-set/UBO/bind-descriptor-sets/draw/copy/submit"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // --- assert the ACTUAL lavapipe raster: the UBO translation pushed the triangle RIGHT --------
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no 64x64 RGBA8 render target captured off the host executor. Captured texture sizes: {:?}",
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
    let is_green = |c: [u8; 4]| c[1] > 180 && c[0] < 90 && c[2] < 90;
    let red = [255u8, 0, 0, 255];

    // Count GREEN in the left vs right half of the framebuffer.
    let (mut left_green, mut right_green) = (0usize, 0usize);
    for y in 0..H {
        for x in 0..W {
            if is_green(texel(x, y)) {
                if x < W / 2 {
                    left_green += 1;
                } else {
                    right_green += 1;
                }
            }
        }
    }
    eprintln!(
        "green pixels: left={left_green} right={right_green}; center={:?}",
        texel(W / 2, H / 2)
    );

    // The uniform translation (+0.9 in x) must have pushed the triangle into the RIGHT half...
    assert!(
        right_green > 100,
        "the UBO-translated triangle should cover a chunk of the RIGHT half (got {right_green} green px)"
    );
    // ...and left almost nothing (the triangle started centered; only the uniform moved it right).
    assert!(
        left_green < 20,
        "the LEFT half should be essentially clear — the uniform moved the triangle away from center \
         (got {left_green} green px)"
    );
    // The center, where the un-translated triangle WOULD have drawn, is back to the RED clear — this is
    // the crux: it proves the uniform VALUE (not a hardcoded position) determined where geometry landed.
    assert_eq!(
        texel(W / 2, H / 2),
        red,
        "center should be the RED clear — the UBO translation moved the triangle off-center"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
