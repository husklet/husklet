//! EXACT-PIXEL rasterization proof for EVERY primitive topology the protocol advertises.
//!
//! The protocol's [`Topology`] enum (WebGPU numbering) advertises exactly five assembly modes —
//! `PointList=0, LineList=1, LineStrip=2, TriangleList=3, TriangleStrip=4` — and the wgpu executor maps
//! each to its `wgpu::PrimitiveTopology` counterpart 1:1 in `pipeline.rs::topology`. `geometry_demo.rs`
//! already exercises the TRIANGLE path exhaustively (transformed/instanced quads, exact rects); this file
//! is the companion that proves the OTHER four topologies rasterize to the exact right pixels, and
//! re-covers TriangleList/TriangleStrip as a control so all five are asserted in one place.
//!
//! Every draw feeds NDC positions straight from a vertex buffer (no matrix) with a flat-white fragment,
//! into a small `TARGET`×`TARGET` render target, and asserts the readback pixel-for-pixel against a mask
//! computed from the *documented* WebGPU/Vulkan rasterization rules — not from the observed output. A
//! primitive that lands one pixel over, drops a pixel, or fills the wrong span fails.
//!
//! ## The rasterization rules this test bakes in (WebGPU + Vulkan "basic" line/point rules, 1px, no AA)
//!
//! wgpu never enables `VK_EXT_line_rasterization`, so points and lines use the Vulkan *Basic* rules with
//! `lineWidth`/`pointSize` fixed at 1.0 (WebGPU has no width control, and no MSAA here → no AA):
//!
//! * **Point** (size 1): covers the single fragment whose pixel square contains the point. A point placed
//!   at a pixel CENTER `(px+0.5, py+0.5)` lights exactly pixel `(px,py)`; neighbours' centers sit a full
//!   unit away, outside the 0.5 half-extent, so no bleed. Verified: 4 points → 4 isolated pixels.
//! * **Line** (Bresenham, width 1, diamond-exit): one fragment per step along the major axis, and the
//!   segment is half-open at ONE endpoint. Which endpoint is dropped is NOT the traversal order — it is a
//!   consistent top-left-style tie-break: the endpoint with the greater x is excluded, and for a vertical
//!   segment (equal x) the endpoint with the greater y (the lower one) is excluded. Verified against
//!   lavapipe by drawing each direction as its own segment: `(3,8)->(12,8)` AND `(12,8)->(3,8)` BOTH lit
//!   cols 3..=11 (col 12, the max-x end, dropped either way); `(8,3)->(8,12)` and `(8,12)->(8,3)` both lit
//!   rows 3..=11 (row 12, the max-y end, dropped); the 45° `(2,2)->(9,9)` lit (2,2)..=(8,8) (the max-x
//!   corner (9,9) dropped). This half-open rule is what makes a `LineStrip`'s shared vertex paint exactly
//!   once — but note a direction REVERSAL can leave a 1-pixel hole where a corner is the max endpoint of
//!   BOTH adjacent segments, so this test uses a monotone (rightward/downward) staircase strip where every
//!   corner is one segment's max (dropped) and the next's min (kept) → painted once, no hole.
//! * **Triangle** (top-left fill rule, sample at pixel center): a fragment is generated iff the pixel
//!   center lies inside the triangle, with the top-left tie-break on shared edges — so a `TriangleStrip`
//!   quad and the equivalent two-triangle `TriangleList` fill the SAME solid rect with no seam and no
//!   double-cover. `trianglestrip_quad_equals_trianglelist` asserts that byte-for-byte.
//!
//! If NO adapter is reachable (no lavapipe/Vulkan ICD) every test skips, mirroring the rest of the suite.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, ColorAttachment, ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc,
    VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const TARGET: u32 = 32;
const OUT_DIR: &str = "/tmp/hl-demo";

// Flat opaque white for every drawn primitive; the clear is opaque black. The two are maximally apart so
// the bright/clear classification (and the ±2/channel exact compare) is never in doubt.
const FILL: [u8; 4] = [255, 255, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 255];

// Packed vertex-attribute wire format (comps | kind<<8 | norm<<16); a 2-component f32 → comps=2, kind=0.
const VFMT_F32X2: u32 = 2;

// ---------------------------------------------------------------------------------------------------
// tiny shared helpers (mirrors geometry_demo.rs)
// ---------------------------------------------------------------------------------------------------

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.to_string(),
        source: source.to_string(),
    }
    .to_words()
}
fn le_f32(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|f| f.to_le_bytes()).collect()
}
fn new_session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

/// NDC position whose framebuffer projection is the CENTER of pixel `(px,py)` (origin top-left, y-down as
/// the readback rows are). Placing every vertex on a pixel center is what makes point/line/triangle
/// sampling unambiguous — no sample ever lands on a primitive edge.
fn center_ndc(px: i32, py: i32) -> [f32; 2] {
    let x = (px as f32 + 0.5) / TARGET as f32 * 2.0 - 1.0;
    let y = 1.0 - (py as f32 + 0.5) / TARGET as f32 * 2.0;
    [x, y]
}

// Positions-in, flat-white-out. The vertex position IS the NDC coordinate, so the rasterizer input is
// exactly what the test specifies — nothing between the model and the pixels.
const VS: &str = r#"#version 460
layout(location = 0) in vec2 pos;
void main() { gl_Position = vec4(pos, 0.0, 1.0); }
"#;
const FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(1.0, 1.0, 1.0, 1.0); }
"#;

/// Mint one render pass drawing `verts` (as NDC pixel-center coords) with the given topology into a fresh
/// `TARGET`×`TARGET` Rgba8 target, and return the readback bytes.
fn draw(topo: Topology, verts: &[[f32; 2]]) -> Vec<u8> {
    let mut exec =
        WgpuExecutor::new(DeviceConfig::default()).expect("adapter already probed reachable");
    let mut s = new_session(&exec);
    let data: Vec<f32> = verts.iter().flatten().copied().collect();
    let n = verts.len() as u32;
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                TextureDesc {
                    width: TARGET,
                    height: TARGET,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: (data.len() * 4) as u64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&data),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 8,
                        step_mode: 0,
                        attrs: vec![VertexAttr {
                            location: 0,
                            format: VFMT_F32X2,
                            offset: 0,
                        }],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: None,
                    topology: topo,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 1,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: n,
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
    .expect("the topology draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

// ---------------------------------------------------------------------------------------------------
// coverage model — the EXACT lit pixels each topology must produce (from the rules documented above)
// ---------------------------------------------------------------------------------------------------

/// A `TARGET*TARGET` boolean mask, row-major (readback order).
type Mask = Vec<bool>;

fn empty_mask() -> Mask {
    vec![false; (TARGET * TARGET) as usize]
}
fn set(mask: &mut Mask, px: i32, py: i32) {
    assert!(
        (0..TARGET as i32).contains(&px) && (0..TARGET as i32).contains(&py),
        "coverage model put a pixel ({px},{py}) off the {TARGET}×{TARGET} target — the test geometry is wrong"
    );
    mask[(py * TARGET as i32 + px) as usize] = true;
}

/// The Bresenham fragments for ONE line segment between integer pixel centers `a`→`b`. The segment is
/// half-open at ONE endpoint per the diamond-exit tie-break the executor's device exhibits: the endpoint
/// with the greater x is dropped, and for a vertical segment (equal x) the endpoint with the greater y is
/// dropped — independent of the order `a`/`b` are given. Restricted to axis-aligned or exact-45° segments
/// so the staircase is a closed form (one step per major-axis unit); a general slope would need full
/// Bresenham error tracking, which we don't use — asserted here so a future non-45° line can't mis-model.
fn line_pixels(a: (i32, i32), b: (i32, i32)) -> Vec<(i32, i32)> {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    assert!(
        dx == 0 || dy == 0 || dx.abs() == dy.abs(),
        "line_pixels only models axis-aligned or exact-45° segments (got d=({dx},{dy}))"
    );
    // The excluded endpoint: greater x, or (when vertical) greater y.
    let excluded = if a.0 != b.0 {
        if a.0 > b.0 {
            a
        } else {
            b
        }
    } else if a.1 > b.1 {
        a
    } else {
        b
    };
    let steps = dx.abs().max(dy.abs());
    let (sx, sy) = (dx.signum(), dy.signum());
    // Full inclusive staircase, minus the single excluded endpoint.
    (0..=steps)
        .map(|i| (a.0 + i * sx, a.1 + i * sy))
        .filter(|&p| p != excluded)
        .collect()
}

/// Mask of a LineList: independent segments from consecutive vertex PAIRS.
fn linelist_mask(verts: &[(i32, i32)]) -> Mask {
    let mut m = empty_mask();
    for pair in verts.chunks_exact(2) {
        for (x, y) in line_pixels(pair[0], pair[1]) {
            set(&mut m, x, y);
        }
    }
    m
}

/// Mask of a LineStrip: a connected chain over consecutive vertices. For the monotone (rightward/downward)
/// staircase this test uses, each shared corner is the excluded (max) endpoint of its incoming segment and
/// the kept (min) endpoint of its outgoing segment, so it is painted exactly once; only the final vertex of
/// the whole chain is dropped.
fn linestrip_mask(verts: &[(i32, i32)]) -> Mask {
    let mut m = empty_mask();
    for pair in verts.windows(2) {
        for (x, y) in line_pixels(pair[0], pair[1]) {
            set(&mut m, x, y);
        }
    }
    m
}

/// Mask of an axis-aligned solid quad whose two opposite corners are pixel centers `(x0,y0)`/`(x1,y1)`.
/// Top-left fill rule at pixel centers: pixel `p` is covered iff its center lies in `[min+0.5, max+0.5)`,
/// i.e. columns `min.x..max.x` and rows `min.y..max.y` (the far edge is exclusive).
fn quad_mask(c0: (i32, i32), c1: (i32, i32)) -> Mask {
    let (x0, x1) = (c0.0.min(c1.0), c0.0.max(c1.0));
    let (y0, y1) = (c0.1.min(c1.1), c0.1.max(c1.1));
    let mut m = empty_mask();
    for py in y0..y1 {
        for px in x0..x1 {
            set(&mut m, px, py);
        }
    }
    m
}

// ---------------------------------------------------------------------------------------------------
// exact assertion + PNG dump (adapted from geometry_demo.rs)
// ---------------------------------------------------------------------------------------------------

fn ascii(mask_or_px: impl Fn(u32) -> bool) -> String {
    let mut s = String::new();
    for py in 0..TARGET {
        for px in 0..TARGET {
            s.push(if mask_or_px(py * TARGET + px) {
                '#'
            } else {
                '.'
            });
        }
        s.push('\n');
    }
    s
}

/// Every pixel in `mask` must equal `FILL`; every other pixel must equal `CLEAR`. Writes the PNG first,
/// then on any mismatch dumps actual-vs-expected ASCII and panics with the first offending pixel.
fn assert_exact_and_write(name: &str, px: &[u8], mask: &Mask) {
    write_png(name, px);
    let approx = |a: &[u8], b: [u8; 4]| {
        (a[0] as i16 - b[0] as i16).abs() <= 2
            && (a[1] as i16 - b[1] as i16).abs() <= 2
            && (a[2] as i16 - b[2] as i16).abs() <= 2
            && (a[3] as i16 - b[3] as i16).abs() <= 2
    };
    let mut bad = 0usize;
    let mut first: Option<(u32, u32, [u8; 4], [u8; 4])> = None;
    for i in 0..(TARGET * TARGET) as usize {
        let p = &px[i * 4..i * 4 + 4];
        let want = if mask[i] { FILL } else { CLEAR };
        if !approx(p, want) {
            bad += 1;
            if first.is_none() {
                first = Some((
                    (i as u32) % TARGET,
                    (i as u32) / TARGET,
                    [p[0], p[1], p[2], p[3]],
                    want,
                ));
            }
        }
    }
    if bad != 0 {
        let bright = |i: u32| {
            let p = &px[(i * 4) as usize..];
            p[0] as u16 + p[1] as u16 + p[2] as u16 > 200
        };
        eprintln!("=== topology `{name}` FAILED exact-pixel check: {bad} wrong pixels ===");
        eprintln!("--- ACTUAL (bright=#) ---\n{}", ascii(bright));
        eprintln!(
            "--- EXPECTED (covered=#) ---\n{}",
            ascii(|i| mask[i as usize])
        );
        let (x, y, got, want) = first.unwrap();
        panic!("topology `{name}`: {bad} pixels differ; first at ({x},{y}) got {got:?} want {want:?} (PNG at {OUT_DIR}/{name}.png)");
    }
    eprintln!("topology `{name}`: exact-pixel match OK — PNG at {OUT_DIR}/{name}.png");
}

// ---------------------------------------------------------------------------------------------------
// PointList — N points at known centers each light exactly one pixel; between stays clear
// ---------------------------------------------------------------------------------------------------

fn write_png(name: &str, rgba: &[u8]) {
    let _ = std::fs::create_dir_all(OUT_DIR);
    let path = format!("{OUT_DIR}/{name}.png");
    let bytes = encode_png(TARGET, TARGET, rgba);
    if let Err(e) = std::fs::write(&path, &bytes) {
        eprintln!("warning: could not write {path}: {e}");
    }
}
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b): (u32, u32) = (1, 0);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut pos = 0usize;
    while pos < raw.len() {
        let chunk = (raw.len() - pos).min(0xFFFF);
        let final_block = pos + chunk >= raw.len();
        out.push(if final_block { 1 } else { 0 });
        out.extend_from_slice(&(chunk as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
        out.extend_from_slice(&raw[pos..pos + chunk]);
        pos += chunk;
    }
    if raw.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}
fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((w * 4 + 1) * h) as usize);
    for y in 0..h {
        raw.push(0);
        let row = (y * w * 4) as usize;
        raw.extend_from_slice(&rgba[row..row + (w * 4) as usize]);
    }
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6);
    ihdr.extend_from_slice(&[0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut png, b"IEND", &[]);
    png
}

#[path = "topology/line.rs"]
mod line;
#[path = "topology/point.rs"]
mod point;
#[path = "topology/strip.rs"]
mod strip;
#[path = "topology/triangle.rs"]
mod triangle;
