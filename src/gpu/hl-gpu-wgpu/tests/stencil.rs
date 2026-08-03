//! Exact-pixel stencil test/write demo — the end-to-end proof that the protocol's stencil surface
//! (`StencilFaceState` front/back + read/write masks on `DepthState`, `clear_stencil` on `DepthAttachment`,
//! and the dynamic `SetStencilReference` op) is really lowered by the wgpu executor and GATES a draw.
//!
//! The IR is two render passes over one color target + one `Depth24PlusStencil8` depth/stencil target:
//!
//!   * Pass A (MARK): clear stencil to 0, draw a CENTERED rect with a pipeline whose stencil is
//!     `compare = ALWAYS, pass_op = REPLACE, write_mask = 0xFF` and a stencil reference of 1. This writes
//!     the value 1 into the stencil plane for exactly the rect's pixels, 0 everywhere else. (The rect's
//!     color is irrelevant — pass B re-clears the color target.)
//!   * Pass B (TEST): re-clear the COLOR target to blue but LOAD (preserve) the stencil, then draw a
//!     FULLSCREEN triangle in green through a pipeline whose stencil is `compare = EQUAL` against the same
//!     reference 1. Only the fragments whose stored stencil == 1 (the marked rect) pass the test and get
//!     green; everything else keeps the blue clear.
//!
//! The assertion is EXACT: with the stencil test ENABLED, a pixel is green iff it lies in the marked rect
//! (exactly 64 of the 256 pixels), and blue otherwise. The regression control re-runs the SAME two passes
//! with pass B's stencil DISABLED — the whole screen is then green (256/256), proving the stencil is what
//! gated the draw, not the geometry. A PNG of the enabled result is written to `/tmp/hl-demo/` for a visual
//! confrontation of the gated draw.

use std::io::Write;

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, ColorTargetState, DepthAttachment, DepthState, RenderPipelineDesc, ShaderRef,
    StencilFaceState, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    compare, stencil_op, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const N: u32 = 16; // NxN render target
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// The centered rect a `[-0.5, 0.5]^2` NDC quad rasterizes to on an `N`x`N` target: a pixel center at
/// column/row `c` sits at NDC `(c + 0.5) / N * 2 - 1`, which lies in `(-0.5, 0.5)` exactly for `c` in
/// `[N/4, 3N/4)`. For `N = 16` that is `[4, 12)` on each axis — an 8x8 = 64-pixel block dead center, with no
/// pixel center landing on the `±0.5` edge (so the count is unambiguous).
fn inside_rect(x: u32, y: u32) -> bool {
    (N / 4..3 * N / 4).contains(&x) && (N / 4..3 * N / 4).contains(&y)
}

// Vertex shader: a centered quad (two triangles) spanning NDC [-0.5, 0.5]^2, indexed by vertex_index.
const MARK_WGSL: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5), vec2<f32>( 0.5, -0.5), vec2<f32>(-0.5,  0.5),
        vec2<f32>( 0.5, -0.5), vec2<f32>( 0.5,  0.5), vec2<f32>(-0.5,  0.5),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}
@fragment
fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
"#;

// Vertex shader: a fullscreen triangle; fragment outputs green (the pass-B draw color).
const TEST_WGSL: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}
@fragment
fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
"#;

/// Mint SPIR-V (all entry points) from a WGSL seed via naga — the guest SPIR-V ABI round trip.
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

fn tex(fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: N,
        height: N,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}

/// One stencil face: `compare` op + `pass_op` (fail / depth-fail stay `KEEP`).
fn face(cmp: u32, pass: u32) -> StencilFaceState {
    StencilFaceState {
        compare: cmp,
        fail_op: stencil_op::KEEP,
        depth_fail_op: stencil_op::KEEP,
        pass_op: pass,
    }
}

fn pipeline(module: u32, depth: DepthState, topology: Topology) -> RenderPipelineDesc {
    RenderPipelineDesc {
        vertex: ShaderRef {
            module,
            entry: "vs_main".into(),
        },
        fragment: Some(ShaderRef {
            module,
            entry: "fs_main".into(),
        }),
        vertex_buffers: vec![],
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: 0xF,
        }],
        depth: Some(depth),
        topology,
        cull: 0,
        front_face: 0,
        sample_count: 1,
        label: String::new(),
    }
}

/// Run the two-pass mark-then-test IR and return the color plane. `stencil_enabled` selects pass B's
/// stencil: `EQUAL` (gated) vs fully `DISABLED` (the regression control — draws everywhere).
fn run(exec: &mut WgpuExecutor, stencil_enabled: bool) -> Vec<u8> {
    let ds_fmt = TextureFormat::Depth24PlusStencil8;

    // Pass A pipeline: mark the rect — ALWAYS compare, REPLACE on pass, write the whole 0xFF stencil mask.
    let mark_depth = DepthState {
        format: ds_fmt,
        depth_write: false,
        depth_compare: compare::ALWAYS,
        stencil_front: face(compare::ALWAYS, stencil_op::REPLACE),
        stencil_back: face(compare::ALWAYS, stencil_op::REPLACE),
        stencil_read_mask: 0xFF,
        stencil_write_mask: 0xFF,
        bias_constant: 0,
        bias_slope_scale: 0.0,
        bias_clamp: 0.0,
    };
    // Pass B pipeline: test EQUAL(ref) when enabled; fully disabled (IGNORE + zero masks) for the control.
    let test_depth = if stencil_enabled {
        DepthState {
            format: ds_fmt,
            depth_write: false,
            depth_compare: compare::ALWAYS,
            stencil_front: face(compare::EQUAL, stencil_op::KEEP),
            stencil_back: face(compare::EQUAL, stencil_op::KEEP),
            stencil_read_mask: 0xFF,
            stencil_write_mask: 0x00,
            bias_constant: 0,
            bias_slope_scale: 0.0,
            bias_clamp: 0.0,
        }
    } else {
        DepthState {
            format: ds_fmt,
            depth_write: false,
            depth_compare: compare::ALWAYS,
            stencil_front: StencilFaceState::DISABLED,
            stencil_back: StencilFaceState::DISABLED,
            stencil_read_mask: 0x00,
            stencil_write_mask: 0x00,
            bias_constant: 0,
            bias_slope_scale: 0.0,
            bias_clamp: 0.0,
        }
    };

    let caps = exec.capabilities();
    let limits = Limits::from_capabilities(caps);
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateTexture(2, tex(ds_fmt, texture_usage::RENDER_TARGET)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: wgsl_to_spirv(MARK_WGSL),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::SpirV,
                spirv: wgsl_to_spirv(TEST_WGSL),
            },
            Cmd::CreateRenderPipeline(1, pipeline(1, mark_depth, Topology::TriangleList)),
            Cmd::CreateRenderPipeline(2, pipeline(2, test_depth, Topology::TriangleList)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // Pass A — write the stencil buffer (reference 1) under the centered rect.
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: Some(DepthAttachment {
                            texture: 2,
                            depth_load: LoadOp::Clear,
                            stencil_load: LoadOp::Clear,
                            clear_depth: 1.0,
                            clear_stencil: 0,
                        }),
                    },
                    Enc::SetPipeline(1),
                    Enc::SetStencilReference { reference: 1 },
                    Enc::Draw {
                        vertex_count: 6,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                    // Pass B — re-clear color to blue, LOAD (preserve) the stencil, draw fullscreen green
                    // gated by stencil EQUAL 1. Only the marked rect passes.
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 1.0, 1.0],
                            store: true,
                        }],
                        depth: Some(DepthAttachment {
                            texture: 2,
                            depth_load: LoadOp::Load,
                            stencil_load: LoadOp::Load,
                            clear_depth: 1.0,
                            clear_stencil: 0,
                        }),
                    },
                    Enc::SetPipeline(2),
                    Enc::SetStencilReference { reference: 1 },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("two-pass stencil mark+test IR must run cleanly");

    exec.read_texture(&s.resources, 1)
        .expect("read color target")
}

fn count_color(px: &[u8], want: [u8; 4]) -> usize {
    px.chunks_exact(4).filter(|c| *c == want).count()
}

#[test]
fn stencil_test_gates_the_draw_to_the_marked_rect() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    // ---- ENABLED: only the marked rect is green; everything else is the pass-B blue clear. ----
    let enabled = run(&mut exec, true);
    write_png("/tmp/hl-demo/stencil_gated.png", &enabled);

    for y in 0..N {
        for x in 0..N {
            let px = &enabled[((y * N + x) * 4) as usize..][..4];
            let want = if inside_rect(x, y) { GREEN } else { BLUE };
            assert_eq!(
                px, want,
                "pixel ({x},{y}) inside_rect={} must be {want:?}; the stencil EQUAL test did not gate the \
                 draw to the marked rect",
                inside_rect(x, y)
            );
        }
    }
    let green_enabled = count_color(&enabled, GREEN);
    assert_eq!(
        green_enabled, 64,
        "exactly the 8x8 = 64 marked-rect pixels must be green (got {green_enabled}); a full-screen fill \
         (256) would mean the stencil never gated the draw"
    );
    assert_eq!(
        count_color(&enabled, BLUE),
        (N * N) as usize - 64,
        "the rest must be the blue clear"
    );

    // ---- DISABLED control: the SAME geometry with pass B's stencil off floods the whole screen green. ----
    let disabled = run(&mut exec, false);
    let green_disabled = count_color(&disabled, GREEN);
    assert_eq!(
        green_disabled, (N * N) as usize,
        "with the stencil test DISABLED the fullscreen draw must cover ALL {} pixels ({green_disabled} \
         green) — this is the regression proof: the ONLY difference from the enabled run is the stencil \
         state, so the 64-vs-256 gap is caused by the stencil test itself",
        N * N
    );

    // The two runs must actually differ — a stencil that changed nothing would fail the whole point.
    assert!(
        green_enabled < green_disabled,
        "enabled ({green_enabled}) must gate strictly fewer pixels than disabled ({green_disabled})"
    );
}

// -------------------------------------------------------------------------------------------------
// Minimal dependency-free PNG writer (uncompressed/stored zlib) — for a visual confrontation of the
// gated draw. Upscales each texel to a SCALE x SCALE block so the 16x16 result is legible.
// -------------------------------------------------------------------------------------------------

fn write_png(path: &str, rgba: &[u8]) {
    const SCALE: u32 = 24;
    let (w, h) = (N * SCALE, N * SCALE);
    // Upscale nearest-neighbor into a w*h RGBA buffer.
    let mut up = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let sx = x / SCALE;
            let sy = y / SCALE;
            let src = ((sy * N + sx) * 4) as usize;
            let dst = ((y * w + x) * 4) as usize;
            up[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::File::create(path) {
        Ok(mut f) => {
            let bytes = encode_png(w, h, &up);
            let _ = f.write_all(&bytes);
            eprintln!("stencil demo PNG written: {path} ({w}x{h})");
        }
        Err(e) => eprintln!("could not write {path}: {e}"),
    }
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    // Raw filtered scanlines: one 0x00 (no filter) byte per row, then the row's RGBA bytes.
    let mut raw = Vec::with_capacity((h * (1 + w * 4)) as usize);
    for y in 0..h {
        raw.push(0);
        let row = (y * w * 4) as usize;
        raw.extend_from_slice(&rgba[row..row + (w * 4) as usize]);
    }
    let idat = zlib_stored(&raw);

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, deflate, no filter/interlace
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"IDAT", &idat);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

/// A zlib stream wrapping `data` in uncompressed (stored) DEFLATE blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib header (deflate, default window)
    let mut i = 0;
    while i < data.len() {
        let chunk = (data.len() - i).min(0xFFFF);
        let last = i + chunk >= data.len();
        out.push(if last { 1 } else { 0 }); // BFINAL, BTYPE=00 (stored)
        out.extend_from_slice(&(chunk as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
        out.extend_from_slice(&data[i..i + chunk]);
        i += chunk;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn write_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    png.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
