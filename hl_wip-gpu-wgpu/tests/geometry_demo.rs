//! Self-owned GEOMETRY-CORRECTNESS demos for the wgpu executor.
//!
//! These demos exist to answer ONE question with exact pixels: does this executor rasterize geometry to
//! the CORRECT screen positions? A big app's screenshot exposed DEGENERATE geometry — huge triangles at
//! wrong positions — that luminance-spread heuristics (see `instanced_storage.rs`) sail straight past: a
//! quad rendered huge, or collapsed to the origin, still spreads luminance. So every assert here is an
//! EXACT pixel rectangle: the covered pixels must equal the quad color and every other pixel must equal
//! the clear color. A quad that lands one quadrant over, or fills the whole target, fails.
//!
//! Each demo also writes its frame to `/tmp/hl-demo/<name>.png` (a tiny built-in uncompressed-PNG encoder)
//! for human visual confirmation, and prints an ASCII coverage map to stderr on the diagnostic path.
//!
//! The three demos model the shapes the real bug lives in (GPUI / instanced quads):
//!   1. `transform_quad_lands_at_known_rect` — a unit quad transformed by a std140 mat4 MVP uniform
//!      (identity / translate / scale / 90° rotate); each asserts the quad occupies EXACTLY where the
//!      matrix puts it and the rest is clear.
//!   2. `instanced_vertex_index_quads_from_storage` — THE GPUI repro: a unit quad synthesized from
//!      `@builtin(vertex_index)`, offset+sized PER-INSTANCE by a set-1 STORAGE buffer of {vec2 pos, vec2
//!      size} indexed by `@builtin(instance_index)`; 4 instances at a 2×2 grid, each a distinct color.
//!   3. `instanced_from_vertex_buffer` — the same grid but per-instance data from a step_mode=Instance
//!      vertex buffer, to isolate storage-vs-vertex-buffer.
//!
//! If NO adapter is reachable (no lavapipe/Vulkan ICD) the tests skip, mirroring the rest of the suite.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, ShaderRef, TextureDesc, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 64;
const H: u32 = 64;
const OUT_DIR: &str = "/tmp/hl-demo";

// A distinct opaque color per role, chosen so no two are within the brightness threshold of each other.
const CLEAR: [u8; 4] = [0, 0, 0, 255];
const RED: [u8; 4] = [220, 40, 40, 255];
const GREEN: [u8; 4] = [40, 200, 60, 255];
const BLUE: [u8; 4] = [50, 90, 230, 255];
const WHITE: [u8; 4] = [240, 240, 240, 255];

// ---------------------------------------------------------------------------------------------------
// tiny helpers
// ---------------------------------------------------------------------------------------------------

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor { stage, entry: entry.to_string(), source: source.to_string() }.to_words()
}

fn tex(w: u32, h: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn le_f32(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn color_target() -> ColorTargetState {
    ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }
}

fn new_session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

/// A 2D affine (`new = [[a,b],[c,d]] * xy + [tx,ty]`) as a column-major std140 mat4 — exactly what a
/// GLSL `mat4` uniform consumes when the vertex does `mvp * vec4(local, 0, 1)`.
fn affine_mat4(a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) -> [f32; 16] {
    [
        a, c, 0.0, 0.0, // column 0
        b, d, 0.0, 0.0, // column 1
        0.0, 0.0, 1.0, 0.0, // column 2
        tx, ty, 0.0, 1.0, // column 3
    ]
}

// ---------------------------------------------------------------------------------------------------
// coverage model — the TRUE expected rect from the rasterization sample rule
// ---------------------------------------------------------------------------------------------------

/// A rectangle in FRAMEBUFFER space (x right, y DOWN, origin top-left — the readback's row order).
#[derive(Clone, Copy, Debug)]
struct FbRect {
    x0: f32,
    x1: f32,
    y0: f32, // top
    y1: f32, // bottom
}

/// Map an NDC rectangle (`x∈[nx0,nx1]`, `y∈[ny0,ny1]`, y-UP as wgpu presents to the shader) to the
/// framebuffer rectangle it rasterizes into. wgpu NDC: x=-1 left, x=+1 right, y=+1 TOP, y=-1 bottom; the
/// readback's row 0 is the top of the texture, so a larger NDC y is a SMALLER framebuffer row.
fn ndc_to_fb(nx0: f32, nx1: f32, ny0: f32, ny1: f32) -> FbRect {
    let fx = |n: f32| (n + 1.0) / 2.0 * W as f32;
    let fy = |n: f32| (1.0 - n) / 2.0 * H as f32; // y-up NDC → y-down framebuffer
    FbRect {
        x0: fx(nx0.min(nx1)),
        x1: fx(nx0.max(nx1)),
        y0: fy(ny0.max(ny1)), // top row = larger NDC y
        y1: fy(ny0.min(ny1)),
    }
}

/// The exact set of covered pixels for a union of framebuffer rects, by the standard sample-point rule: a
/// pixel `(px,py)` is covered iff its center `(px+0.5, py+0.5)` lies strictly inside a rect. Our rects have
/// integer edges and centers land on half-integers, so no sample ever sits on an edge — coverage is exact,
/// no fill-rule ambiguity. Returns a `W*H` boolean mask in row-major (readback) order.
fn covered_mask(rects: &[FbRect]) -> Vec<bool> {
    let mut m = vec![false; (W * H) as usize];
    for py in 0..H {
        for px in 0..W {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            let hit = rects.iter().any(|r| cx > r.x0 && cx < r.x1 && cy > r.y0 && cy < r.y1);
            m[(py * W + px) as usize] = hit;
        }
    }
    m
}

/// Render an ASCII coverage map of the actual pixels (`#` = bright, `.` = clear) — the human-legible dump
/// that shows a huge/mis-placed/collapsed quad at a glance.
fn ascii_actual(px: &[u8]) -> String {
    let mut s = String::with_capacity(((W + 1) * H) as usize);
    for py in 0..H {
        for pxi in 0..W {
            let p = &px[((py * W + pxi) * 4) as usize..];
            let bright = p[0] as u16 + p[1] as u16 + p[2] as u16 > 200;
            s.push(if bright { '#' } else { '.' });
        }
        s.push('\n');
    }
    s
}

/// EXACT-pixel assertion + PNG dump. Every pixel in `mask` must equal `fill`; every other pixel must equal
/// `CLEAR`. On mismatch, prints the actual vs expected ASCII maps (so a huge/shifted quad is obvious) and
/// panics. Always writes `/tmp/hl-demo/<name>.png` for visual confirmation first.
fn assert_exact_and_write(name: &str, px: &[u8], mask: &[bool], fill: [u8; 4]) {
    write_png(name, px);

    let approx = |a: &[u8], b: [u8; 4]| {
        // lavapipe's flat, unlit, unblended fill is exact; allow ±2/channel for any rounding only.
        (a[0] as i16 - b[0] as i16).abs() <= 2
            && (a[1] as i16 - b[1] as i16).abs() <= 2
            && (a[2] as i16 - b[2] as i16).abs() <= 2
            && (a[3] as i16 - b[3] as i16).abs() <= 2
    };

    let mut bad = 0usize;
    let mut first: Option<(u32, u32, [u8; 4], [u8; 4])> = None;
    for i in 0..(W * H) as usize {
        let p = &px[i * 4..i * 4 + 4];
        let want = if mask[i] { fill } else { CLEAR };
        if !approx(p, want) {
            bad += 1;
            if first.is_none() {
                first = Some((
                    (i as u32) % W,
                    (i as u32) / W,
                    [p[0], p[1], p[2], p[3]],
                    want,
                ));
            }
        }
    }

    if bad != 0 {
        // Build the EXPECTED ascii from the mask for a side-by-side.
        let mut exp = String::new();
        for py in 0..H {
            for pxi in 0..W {
                exp.push(if mask[(py * W + pxi) as usize] { '#' } else { '.' });
            }
            exp.push('\n');
        }
        eprintln!("=== demo `{name}` FAILED exact-pixel check: {bad} wrong pixels ===");
        eprintln!("--- ACTUAL (bright=#) ---\n{}", ascii_actual(px));
        eprintln!("--- EXPECTED (covered=#) ---\n{exp}");
        if let Some((x, y, got, want)) = first {
            panic!(
                "demo `{name}`: {bad} pixels differ from the exact expected rect; first at ({x},{y}) \
                 got {got:?} want {want:?} (PNG at {OUT_DIR}/{name}.png)"
            );
        }
    }
    eprintln!("demo `{name}`: exact-pixel match OK — PNG at {OUT_DIR}/{name}.png");
}

// ---------------------------------------------------------------------------------------------------
// demo 1 — a std140 MVP-transformed unit quad lands at the matrix-determined rect
// ---------------------------------------------------------------------------------------------------

// Unit quad in LOCAL space [0,1]² synthesized from gl_VertexIndex (a 4-vertex triangle strip), transformed
// by the set-0 std140 mat4 MVP uniform. This is the GPUI vertex model minus the instancing: geometry comes
// from the vertex index, position from a matrix in a uniform block — the exact path a wrong std140 layout /
// offset or a bad NDC map would corrupt.
const MVP_VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform MVP { mat4 mvp; } u;
void main() {
    vec2 local = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1));
    gl_Position = u.mvp * vec4(local, 0.0, 1.0);
}
"#;

// Fragment: a constant color supplied per draw via a second set-0 uniform (so the fill color is data, not a
// baked constant — proving the covered pixels are the drawn quad).
const CONST_COLOR_FS: &str = r#"#version 460
layout(std140, set = 0, binding = 1) uniform Tint { vec4 color; } t;
layout(location = 0) out vec4 o;
void main() { o = t.color; }
"#;

fn run_mvp_case(exec: &mut WgpuExecutor, mvp: [f32; 16], color: [u8; 4]) -> Vec<u8> {
    let mut s = new_session(exec);
    let col_f = [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        1.0,
    ];
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
            // set 0, binding 0: the mat4 MVP (64 bytes, std140).
            Cmd::CreateBuffer(1, BufferDesc { size: 64, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: le_f32(&mvp) },
            // set 0, binding 1: the tint (16 bytes, std140 vec4).
            Cmd::CreateBuffer(2, BufferDesc { size: 16, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 2, offset: 0, data: le_f32(&col_f) },
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", MVP_VS) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", CONST_COLOR_FS) },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vmain".into() },
                    fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                    vertex_buffers: vec![],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: Topology::TriangleStrip,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 64 } },
                        BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 16 } },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw { vertex_count: 4, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the MVP-transformed quad draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

#[test]
fn transform_quad_lands_at_known_rect() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Each case: an affine → the LOCAL [0,1]² quad's NDC rect → the exact expected framebuffer coverage.
    // identity      → NDC x[0,1] y[0,1]      = top-right quadrant
    // scale 0.5     → NDC x[0,0.5] y[0,0.5]  = a quarter block hugging the center, in the top-right
    // translate     → NDC x[-1,0] y[-1,0]    = bottom-left quadrant
    // rotate 90° CCW→ NDC x[-1,0] y[0,1]     = top-left quadrant

    // identity
    let m = run_mvp_case(&mut exec, affine_mat4(1.0, 0.0, 0.0, 1.0, 0.0, 0.0), RED);
    let mask = covered_mask(&[ndc_to_fb(0.0, 1.0, 0.0, 1.0)]);
    assert_exact_and_write("transform_identity", &m, &mask, RED);

    // uniform scale 0.5 about the origin
    let m = run_mvp_case(&mut exec, affine_mat4(0.5, 0.0, 0.0, 0.5, 0.0, 0.0), GREEN);
    let mask = covered_mask(&[ndc_to_fb(0.0, 0.5, 0.0, 0.5)]);
    assert_exact_and_write("transform_scale", &m, &mask, GREEN);

    // translate by (-1,-1): the [0,1]² quad moves to [-1,0]²
    let m = run_mvp_case(&mut exec, affine_mat4(1.0, 0.0, 0.0, 1.0, -1.0, -1.0), BLUE);
    let mask = covered_mask(&[ndc_to_fb(-1.0, 0.0, -1.0, 0.0)]);
    assert_exact_and_write("transform_translate", &m, &mask, BLUE);

    // rotate 90° CCW about origin: (x,y) → (-y, x). The [0,1]² quad maps to x[-1,0], y[0,1].
    let m = run_mvp_case(&mut exec, affine_mat4(0.0, -1.0, 1.0, 0.0, 0.0, 0.0), WHITE);
    let mask = covered_mask(&[ndc_to_fb(-1.0, 0.0, 0.0, 1.0)]);
    assert_exact_and_write("transform_rotate90", &m, &mask, WHITE);
}

// ---------------------------------------------------------------------------------------------------
// the 2×2 grid shared by demos 2 & 3
// ---------------------------------------------------------------------------------------------------

// Per-instance rectangles as (center.x, center.y, half.x, half.y) in NDC. Four cells of a 2×2 grid, each a
// half-extent of 0.25 with centers at (±0.5, ±0.5): distinct, separated blocks (a clear gutter between
// them), so a collapsed/huge/mis-placed instance is unmistakable.
const GRID: [[f32; 4]; 4] = [
    [-0.5, 0.5, 0.25, 0.25],  // instance 0: top-left    (RED)
    [0.5, 0.5, 0.25, 0.25],   // instance 1: top-right   (GREEN)
    [-0.5, -0.5, 0.25, 0.25], // instance 2: bottom-left (BLUE)
    [0.5, -0.5, 0.25, 0.25],  // instance 3: bottom-right(WHITE)
];
const GRID_COLORS: [[u8; 4]; 4] = [RED, GREEN, BLUE, WHITE];

/// The exact expected coverage of one grid instance (its own cell) as a framebuffer mask.
fn grid_cell_mask(inst: usize) -> Vec<bool> {
    let [cx, cy, hx, hy] = GRID[inst];
    covered_mask(&[ndc_to_fb(cx - hx, cx + hx, cy - hy, cy + hy)])
}

/// Assert a rendered grid: EACH pixel must equal the color of whichever cell covers it, else CLEAR. This is
/// stricter than "4 colored blobs exist" — a quad that leaks outside its cell, or lands in another cell,
/// fails. Writes the combined PNG.
fn assert_grid(name: &str, px: &[u8]) {
    write_png(name, px);
    let masks: Vec<Vec<bool>> = (0..4).map(grid_cell_mask).collect();

    let approx = |a: &[u8], b: [u8; 4]| {
        (a[0] as i16 - b[0] as i16).abs() <= 2
            && (a[1] as i16 - b[1] as i16).abs() <= 2
            && (a[2] as i16 - b[2] as i16).abs() <= 2
            && (a[3] as i16 - b[3] as i16).abs() <= 2
    };

    let mut bad = 0usize;
    let mut first: Option<(u32, u32, [u8; 4], [u8; 4])> = None;
    for i in 0..(W * H) as usize {
        let mut want = CLEAR;
        for c in 0..4 {
            if masks[c][i] {
                want = GRID_COLORS[c];
                break;
            }
        }
        let p = &px[i * 4..i * 4 + 4];
        if !approx(p, want) {
            bad += 1;
            if first.is_none() {
                first = Some(((i as u32) % W, (i as u32) / W, [p[0], p[1], p[2], p[3]], want));
            }
        }
    }
    if bad != 0 {
        eprintln!("=== grid demo `{name}` FAILED: {bad} wrong pixels ===");
        eprintln!("--- ACTUAL (bright=#) ---\n{}", ascii_actual(px));
        let (x, y, got, want) = first.unwrap();
        panic!(
            "grid demo `{name}`: {bad} pixels wrong; first at ({x},{y}) got {got:?} want {want:?} \
             (PNG at {OUT_DIR}/{name}.png) — a quad is huge, collapsed, or in the wrong cell"
        );
    }
    // Also prove every cell actually painted (guard against 'all clear happens to match nothing').
    for c in 0..4 {
        let painted = (0..(W * H) as usize)
            .filter(|&i| masks[c][i])
            .all(|i| approx(&px[i * 4..i * 4 + 4], GRID_COLORS[c]));
        assert!(painted, "grid demo `{name}`: instance {c}'s cell is not fully its color {:?}", GRID_COLORS[c]);
    }
    eprintln!("demo `{name}`: exact 2×2-grid match OK — PNG at {OUT_DIR}/{name}.png");
}

// ---------------------------------------------------------------------------------------------------
// demo 2 — instanced unit-quad from vertex_index, per-instance geometry from a set-1 STORAGE buffer
// ---------------------------------------------------------------------------------------------------

// THE GPUI repro. No vertex buffer. The unit quad is synthesized from gl_VertexIndex; each instance's
// rectangle (center.xy, half.zw, NDC) is read from the set-1 read-only STORAGE buffer at gl_InstanceIndex;
// a set-0 uniform (identity viewport scale, the GPUI "globals") is applied; the per-instance color is
// picked from gl_InstanceIndex and forwarded flat. If the storage stride/offset or the instance_index
// wiring is wrong, a quad renders at the origin or huge — the exact degenerate geometry the screenshot bug
// showed.
const STORAGE_VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform Globals { vec2 scale; } g;
layout(std430, set = 1, binding = 0) readonly buffer Quads { vec4 quads[]; };
layout(location = 0) flat out vec4 vColor;
void main() {
    vec4 q = quads[gl_InstanceIndex];
    vec2 corner = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1)) * 2.0 - 1.0;
    vec2 pos = (q.xy + corner * q.zw) * g.scale;
    vec4 pal[4] = vec4[4](
        vec4(220.0/255.0, 40.0/255.0, 40.0/255.0, 1.0),
        vec4(40.0/255.0, 200.0/255.0, 60.0/255.0, 1.0),
        vec4(50.0/255.0, 90.0/255.0, 230.0/255.0, 1.0),
        vec4(240.0/255.0, 240.0/255.0, 240.0/255.0, 1.0)
    );
    vColor = pal[gl_InstanceIndex];
    gl_Position = vec4(pos, 0.0, 1.0);
}
"#;

const FLAT_COLOR_FS: &str = r#"#version 460
layout(location = 0) flat in vec4 vColor;
layout(location = 0) out vec4 o;
void main() { o = vColor; }
"#;

#[test]
fn instanced_vertex_index_quads_from_storage() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut s = new_session(&exec);

    let quads: Vec<f32> = GRID.iter().flatten().copied().collect(); // 4×vec4
    let scale: [f32; 2] = [1.0, 1.0];

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
            Cmd::CreateBuffer(1, BufferDesc { size: 8, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: le_f32(&scale) },
            Cmd::CreateBuffer(2, BufferDesc { size: 64, usage: buffer_usage::STORAGE, label: String::new() }),
            Cmd::WriteBuffer { id: 2, offset: 0, data: le_f32(&quads) },
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", STORAGE_VS) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", FLAT_COLOR_FS) },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vmain".into() },
                    fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                    vertex_buffers: vec![],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: Topology::TriangleStrip,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc { set: 0, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 8 } }] },
            ),
            Cmd::CreateBindGroup(
                2,
                BindGroupDesc { set: 1, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 2, offset: 0, size: 64 } }] },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::SetBindGroup { index: 1, group: 2 },
                    Enc::Draw { vertex_count: 4, instance_count: 4, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the storage-fed instanced grid draw must run cleanly");

    let px = exec.read_texture(&s.resources, 1).unwrap();
    assert_grid("instanced_storage_grid", &px);
}

// ---------------------------------------------------------------------------------------------------
// demo 3 — the same grid, per-instance data from a step_mode=Instance VERTEX BUFFER
// ---------------------------------------------------------------------------------------------------

// Identical geometry, but each instance's rectangle arrives through a per-instance vertex buffer attribute
// (location 0, a vec4) instead of a storage buffer — isolating the storage path from the vertex-buffer
// path. Same corner-from-vertex_index quad, same instance_index→color, so a divergence between this and
// demo 2 would localize the bug to one of the two per-instance data routes.
const VBUF_VS: &str = r#"#version 460
layout(location = 0) in vec4 rect; // per-instance: center.xy, half.zw
layout(location = 0) flat out vec4 vColor;
void main() {
    vec2 corner = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1)) * 2.0 - 1.0;
    vec2 pos = rect.xy + corner * rect.zw;
    vec4 pal[4] = vec4[4](
        vec4(220.0/255.0, 40.0/255.0, 40.0/255.0, 1.0),
        vec4(40.0/255.0, 200.0/255.0, 60.0/255.0, 1.0),
        vec4(50.0/255.0, 90.0/255.0, 230.0/255.0, 1.0),
        vec4(240.0/255.0, 240.0/255.0, 240.0/255.0, 1.0)
    );
    vColor = pal[gl_InstanceIndex];
    gl_Position = vec4(pos, 0.0, 1.0);
}
"#;

// Packed vertex-attribute format (the GL driver's `vertex_format_wire`): comps | (kind<<8) | (norm<<16).
// A 4-component f32 → comps=4, kind=0 → 4.
const VFMT_F32X4: u32 = 4;

#[test]
fn instanced_from_vertex_buffer() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut s = new_session(&exec);

    let insts: Vec<f32> = GRID.iter().flatten().copied().collect();

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
            // The per-instance vertex buffer: 4 × vec4 (center.xy, half.zw).
            Cmd::CreateBuffer(1, BufferDesc { size: 64, usage: buffer_usage::VERTEX, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: le_f32(&insts) },
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", VBUF_VS) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", FLAT_COLOR_FS) },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vmain".into() },
                    fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                    // ONE per-instance vertex buffer: stride 16, step_mode=1 (Instance), a single vec4 attr.
                    vertex_buffers: vec![VertexLayout {
                        stride: 16,
                        step_mode: 1,
                        attrs: vec![VertexAttr { location: 0, format: VFMT_F32X4, offset: 0 }],
                    }],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: Topology::TriangleStrip,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                    Enc::Draw { vertex_count: 4, instance_count: 4, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the vertex-buffer-fed instanced grid draw must run cleanly");

    let px = exec.read_texture(&s.resources, 1).unwrap();
    assert_grid("instanced_vertexbuffer_grid", &px);
}

// ---------------------------------------------------------------------------------------------------
// tiny built-in PNG encoder (RGBA8, uncompressed/stored DEFLATE) — for human visual confirmation only
// ---------------------------------------------------------------------------------------------------

fn write_png(name: &str, rgba: &[u8]) {
    let _ = std::fs::create_dir_all(OUT_DIR);
    let path = format!("{OUT_DIR}/{name}.png");
    let bytes = encode_png(W, H, rgba);
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

/// Wrap raw bytes in a zlib stream using only STORED (uncompressed) DEFLATE blocks — no compressor needed,
/// still a spec-valid PNG any viewer opens.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib header: CM=8, no dict, default level
    let mut pos = 0usize;
    while pos < raw.len() {
        let chunk = (raw.len() - pos).min(0xFFFF);
        let final_block = pos + chunk >= raw.len();
        out.push(if final_block { 1 } else { 0 }); // BFINAL, BTYPE=00 (stored)
        out.extend_from_slice(&(chunk as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
        out.extend_from_slice(&raw[pos..pos + chunk]);
        pos += chunk;
    }
    // An empty image would emit no block; guard with a final empty stored block.
    if raw.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    // Raw image data = each scanline prefixed by a filter byte (0 = none).
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
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.extend_from_slice(&[0, 0, 0]); // compression, filter, interlace
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut png, b"IEND", &[]);
    png
}
