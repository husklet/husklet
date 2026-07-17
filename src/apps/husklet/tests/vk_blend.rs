//! VK RENDER-CORRECTNESS DEMO #6 — vk_blend: a real C Vulkan program whose graphics pipeline requests a
//! src-alpha-over color blend (VkPipelineColorBlendAttachmentState.blendEnable = VK_TRUE) composites a
//! translucent foreground over an opaque background instead of overwriting it, rasterized on lavapipe.
//!
//! One pipeline, two draws: a FULL-FRAME opaque background quad (color BG, alpha 1.0 — its src-alpha-over
//! blend is a no-op replace) then a CENTERED 50%-alpha foreground quad (color FG). On a 64×64 Rgba8Unorm
//! (linear) target the overlap must be the EXACT straight-alpha composite `bg*(1-a) + fg*a` and the border
//! must stay the background. Writes /tmp/hl-demo/vk_blend.png.
//!
//! Regression proof: the SAME guest is re-run with HL_VK_BLEND=0 (blendEnable = VK_FALSE). The shim then
//! lowers `blend: None` (overwrite) and the overlap becomes the RAW foreground — so the blend-on and
//! blend-off overlap pixels differ, and only the fixed shim yields the composite. Reverting the shim
//! (blend hardcoded to None) collapses blend-on onto the blend-off result and fails this test.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{demo_png_dir, staged_dir, write_png};

const VK_HEADERS: &str = "/Users/x/vk-refs/Vulkan-Headers/include";
const LOADER: &str = "/usr/lib/aarch64-linux-gnu/libvulkan.so.1";

const W: u32 = 64;
const H: u32 = 64;

// Opaque background + translucent foreground. Channels chosen so the 50% average is an exact 8-bit integer.
const BG: [u8; 3] = [200, 100, 40];
const FG: [u8; 3] = [40, 180, 220];

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

/// Run the compiled guest against a fresh executor, return the captured 64×64 RGBA8 target. `blend_on`
/// selects `HL_VK_BLEND` (0 => blendEnable VK_FALSE, the overwrite control).
fn run_guest(
    bin: &std::path::Path,
    icd: &std::path::Path,
    vs_path: &std::path::Path,
    fs_path: &std::path::Path,
    blend_on: bool,
) -> Vec<u8> {
    let exec = WgpuExecutorServer::start(if blend_on {
        "vkblend-on"
    } else {
        "vkblend-off"
    });
    let adapter = exec.adapter_name();
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "must rasterize on the software Vulkan device, got {adapter:?}"
    );

    let run = Command::new(bin)
        .env("VK_ICD_FILENAMES", icd)
        .env("VK_DRIVER_FILES", icd)
        .env("VK_LOADER_LAYERS_DISABLE", "~all~")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_VK_VS_SPV", vs_path)
        .env("HL_VK_FS_SPV", fs_path)
        .env("HL_VK_BLEND", if blend_on { "1" } else { "0" })
        .env("VK_LOADER_DEBUG", "error,warn")
        .output()
        .expect("spawn vk_blend");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- vk_blend (blend_on={blend_on}) stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("VK_BLEND_OK"),
        "guest drove the blended draws"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    let cap = exec.captured();
    cap.rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor")
        .clone()
}

#[test]
fn real_vulkan_alpha_blend_composites_exactly_and_disabled_overwrites() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-vkblend-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("vk_blend");

    // A full-frame background quad (verts 0..5, opaque BG) + a centered foreground quad (verts 6..11, NDC
    // [-0.5,0.5] → pixel cols/rows [16,48), FG @ alpha 0.5). Color+alpha are driven purely by gl_VertexIndex
    // (no vertex/uniform buffer); the fragment paints the straight (non-premultiplied) color.
    let vs_spirv = wgsl_to_spirv(&format!(
        r#"
        struct VOut {{ @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> }};
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {{
            var P = array<vec2<f32>, 12>(
                vec2<f32>(-1.0,  1.0), vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0,  1.0),
                vec2<f32>( 1.0,  1.0), vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0, -1.0),
                vec2<f32>(-0.5,  0.5), vec2<f32>(-0.5, -0.5), vec2<f32>( 0.5,  0.5),
                vec2<f32>( 0.5,  0.5), vec2<f32>(-0.5, -0.5), vec2<f32>( 0.5, -0.5));
            var o: VOut;
            o.pos = vec4<f32>(P[vi], 0.0, 1.0);
            if (vi < 6u) {{
                o.color = vec4<f32>({bgr}, {bgg}, {bgb}, 1.0);
            }} else {{
                o.color = vec4<f32>({fgr}, {fgg}, {fgb}, 0.5);
            }}
            return o;
        }}
    "#,
        bgr = BG[0] as f32 / 255.0,
        bgg = BG[1] as f32 / 255.0,
        bgb = BG[2] as f32 / 255.0,
        fgr = FG[0] as f32 / 255.0,
        fgg = FG[1] as f32 / 255.0,
        fgb = FG[2] as f32 / 255.0,
    ));
    let fs_spirv = wgsl_to_spirv(
        r#"
        @fragment
        fn fs_main(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> { return color; }
    "#,
    );
    let vs_path = out_dir.join("blend.vert.spv");
    let fs_path = out_dir.join("blend.frag.spv");
    write_spv(&vs_path, &vs_spirv);
    write_spv(&fs_path, &fs_spirv);

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-vulkan/tests/fixtures/vk_blend.c"
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

    // ---- run 1: blend ON (the fix); run 2: blend OFF (regression control) ----------------------
    let blended = run_guest(&bin, &icd, &vs_path, &fs_path, true);
    let overwritten = run_guest(&bin, &icd, &vs_path, &fs_path, false);

    write_png(&demo_png_dir().join("vk_blend.png"), W, H, &blended);

    let at = |px: &[u8], x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]] // (R,G,B,A)
    };
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 1;
    let eq = |got: [u8; 4], want: [u8; 4]| {
        near(got[0], want[0])
            && near(got[1], want[1])
            && near(got[2], want[2])
            && near(got[3], want[3])
    };

    // EXACT straight-alpha composite at the overlap for a = 0.5: bg*(1-a) + fg*a = (bg+fg)/2 per channel;
    // alpha = fg.a*fg.a + bg.a*(1-fg.a) = 0.5*0.5 + 1.0*0.5 = 0.75 → round(0.75*255) = 191.
    let composite = [
        ((BG[0] as u16 + FG[0] as u16) / 2) as u8, // 120
        ((BG[1] as u16 + FG[1] as u16) / 2) as u8, // 140
        ((BG[2] as u16 + FG[2] as u16) / 2) as u8, // 130
        191,
    ];
    let bg_opaque = [BG[0], BG[1], BG[2], 255];
    let fg_raw = [FG[0], FG[1], FG[2], 128]; // round(0.5*255) = 127.5 → 128; ±1 covers 127/128

    // --- blend ON: overlap composites (EXACT, confronted against the PNG), border is background --
    let ov_on = at(&blended, 32, 32);
    assert_eq!(
        ov_on, composite,
        "overlap must be the exact composite {composite:?}, got {ov_on:?}"
    );
    for (x, y) in [(8, 8), (56, 8), (8, 56), (56, 56), (32, 6), (6, 32)] {
        let b = at(&blended, x, y);
        assert_eq!(
            b, bg_opaque,
            "border ({x},{y}) must be the opaque background {bg_opaque:?}, got {b:?}"
        );
    }

    // --- blend OFF (regression control): overlap is the RAW foreground (overwrite) --------------
    let ov_off = at(&overwritten, 32, 32);
    assert!(
        eq(ov_off, fg_raw),
        "blend-off overlap must be the raw foreground {fg_raw:?}, got {ov_off:?}"
    );
    let border_off = at(&overwritten, 8, 8);
    assert!(
        eq(border_off, bg_opaque),
        "blend-off border must still be the background, got {border_off:?}"
    );

    // --- the two runs' overlap pixels MUST differ (composite != overwrite) ----------------------
    assert_ne!(
        ov_on, ov_off,
        "blend-on (composite) and blend-off (overwrite) overlaps must differ"
    );

    // The foreground box covers the centered 32×32 region (1024 px). Count exact-composite pixels; a real
    // blend (not a full-frame overwrite, not nothing) lands ~1024 in the overlap.
    let composited = blended
        .chunks_exact(4)
        .filter(|t| {
            near(t[0], composite[0])
                && near(t[1], composite[1])
                && near(t[2], composite[2])
                && near(t[3], composite[3])
        })
        .count();
    assert!(
        (700..1400).contains(&composited),
        "the composited overlap must cover ~1024 px (the centered box), got {composited}"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
