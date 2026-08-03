//! Stencil test/op execution in the CPU rasterizer oracle. A two-pass mark-then-test program is driven
//! through the real runtime pipeline (validate → account → dispatch → execute); the observable color
//! readback proves the oracle now models the WHOLE stencil surface — the `Depth24PlusStencil8` stencil
//! plane, `DepthAttachment.clear_stencil`, the dynamic `Enc::SetStencilReference`, the per-face compare
//! (`StencilFaceState.compare`) + op (`pass_op`) with read/write masks — matching wgpu's semantics. The
//! differential fuzzer cross-checks the same model against a real GPU; this pins it WITHOUT an adapter, so
//! the stencil model is always exercised by `hl-gpu`'s own suite.
//!
//! The oracle is a fixed-function rasterizer (the shader stages are ignored) but a render pipeline still
//! needs its vertex shader module to exist and only KERNEL payloads are advertised, so — as in `depth.rs` —
//! a trivial no-op kernel stands in as the module. The oracle's negotiated depth-format set is depth-only,
//! so the harness widens the SESSION caps to admit the combined depth+stencil format (exactly as the
//! differential does), which changes nothing the oracle computes.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, ColorAttachment, ColorTargetState, DepthAttachment, DepthState, RenderPipelineDesc,
    ShaderRef, StencilFaceState, TextureDesc, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, stencil_op, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{Inst, KernelProgram, KERNEL_MAGIC};
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind, TextureId};

const N: u32 = 8; // NxN target — the centred [-0.5,0.5] quad marks the clean [2,6) 4x4 block.
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f64; 4] = [0.0, 0.0, 1.0, 1.0];
const GREEN_PX: [u8; 4] = [0, 255, 0, 255];
const BLUE_PX: [u8; 4] = [0, 0, 255, 255];

/// A centred pixel is "inside the marked rect" iff its column/row is in `[N/4, 3N/4)` (see the wgpu stencil
/// demo's derivation): a pixel centre at `c` sits at NDC `(c + 0.5)/N * 2 - 1`, strictly inside `(-0.5, 0.5)`
/// exactly for `c` in `[N/4, 3N/4)` when `N` is a multiple of 4.
fn inside_rect(x: u32, y: u32) -> bool {
    (N / 4..3 * N / 4).contains(&x) && (N / 4..3 * N / 4).contains(&y)
}

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}
fn placeholder_shader() -> KernelProgram {
    KernelProgram {
        entry: "vs".into(),
        block: [1, 1, 1],
        params: vec![],
        param_bytes: 0,
        num_regions: 0,
        shared_bytes: 0,
        reg_count: 1,
        insts: vec![Inst::Ret],
    }
}

/// One `pos2 (0..8) + colour (8..24)`, stride-24 vertex (matches the oracle's `stride >= 24` arm).
fn vtx(x: f32, y: f32, c: [f32; 4]) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..4].copy_from_slice(&x.to_le_bytes());
    b[4..8].copy_from_slice(&y.to_le_bytes());
    b[8..12].copy_from_slice(&c[0].to_le_bytes());
    b[12..16].copy_from_slice(&c[1].to_le_bytes());
    b[16..20].copy_from_slice(&c[2].to_le_bytes());
    b[20..24].copy_from_slice(&c[3].to_le_bytes());
    b
}
/// The centred quad (two triangles) spanning NDC `[-0.5, 0.5]^2`, coloured `c`.
fn quad(c: [f32; 4]) -> Vec<u8> {
    let corners = [
        (-0.5, -0.5),
        (0.5, -0.5),
        (-0.5, 0.5),
        (0.5, -0.5),
        (0.5, 0.5),
        (-0.5, 0.5),
    ];
    corners.iter().flat_map(|(x, y)| vtx(*x, *y, c)).collect()
}
/// The fullscreen triangle (covers every pixel centre), coloured `c`.
fn fstri(c: [f32; 4]) -> Vec<u8> {
    [(-1.0, -1.0), (3.0, -1.0), (-1.0, 3.0)]
        .iter()
        .flat_map(|(x, y)| vtx(*x, *y, c))
        .collect()
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
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
fn face(cmp: u32, pass: u32) -> StencilFaceState {
    StencilFaceState {
        compare: cmp,
        fail_op: stencil_op::KEEP,
        depth_fail_op: stencil_op::KEEP,
        pass_op: pass,
    }
}
fn depth_stencil(cmp: u32, pass: u32, write_mask: u32) -> DepthState {
    DepthState {
        format: TextureFormat::Depth24PlusStencil8,
        depth_write: false,
        depth_compare: compare::ALWAYS,
        stencil_front: face(cmp, pass),
        stencil_back: face(cmp, pass),
        stencil_read_mask: 0xFF,
        stencil_write_mask: write_mask,
        bias_constant: 0,
        bias_slope_scale: 0.0,
        bias_clamp: 0.0,
    }
}
fn pipeline(id: u32, depth: DepthState) -> Cmd {
    Cmd::CreateRenderPipeline(
        id,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "vs".into(),
            },
            fragment: None,
            vertex_buffers: vec![VertexLayout {
                stride: 24,
                step_mode: 0,
                attrs: vec![
                    VertexAttr {
                        location: 0,
                        format: 0,
                        offset: 0,
                    },
                    VertexAttr {
                        location: 1,
                        format: 0,
                        offset: 8,
                    },
                ],
            }],
            color_targets: vec![ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: 0xF,
            }],
            depth: Some(depth),
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
        },
    )
}

/// Run a command batch through the oracle with the session widened to admit `Depth24PlusStencil8`, and read
/// back the `N*N` colour plane of texture 1.
fn run(cmds: &[Cmd]) -> Vec<u8> {
    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, placeholder_shader());
    let caps = hl_gpu::Capabilities::oracle_session_fixture(&exec.capabilities());
    let mut limits = hl_gpu::Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = hl_gpu::Session::new(
        limits,
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, cmds).expect("stencil program must run cleanly");
    let mut px = vec![0u8; (N * N * 4) as usize];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    px
}

fn at(px: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * N + x) * 4) as usize;
    px[i..i + 4].try_into().unwrap()
}
fn count(px: &[u8], want: [u8; 4]) -> usize {
    px.chunks_exact(4).filter(|c| *c == want).count()
}

/// Two-pass mark-then-test scaffold shared by the tests. Pass A clears the stencil to 0 and marks with
/// `mark` using `mark_buffer` (`1` = the centred quad → the `[N/4,3N/4)` rect; `2` = the fullscreen
/// triangle → every pixel) drawing `mark_verts` vertices. An optional second mark pass (`mark2`) LOADs the
/// stencil and marks again (for accumulation). The final pass re-clears colour to blue, LOADs the stencil,
/// and draws a fullscreen GREEN triangle through `test` (the gate). Returns the colour plane.
///
/// NOTE the quad (`mark_buffer = 1`) is two triangles sharing the anti-diagonal `x+y=0`; the oracle's
/// two-sided coverage test covers a pixel centre lying exactly on that shared edge with BOTH triangles, so a
/// NON-idempotent stencil op (`INCREMENT`/`INVERT`) would double-count it within one draw. Idempotent ops
/// (`REPLACE`, and `KEEP` on the test) are unaffected — so the quad is used only for `REPLACE` marks; the
/// accumulation test marks with the single fullscreen triangle (`mark_buffer = 2`), which has no shared edge.
#[allow(clippy::too_many_arguments)]
fn mark_then_test(
    reference: u32,
    mark_buffer: u32,
    mark_verts: u32,
    mark: DepthState,
    mark2: Option<DepthState>,
    test: DepthState,
) -> Vec<u8> {
    let mut next_pipe = 1u32;
    let mut cmds = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kernel_words(),
        },
        Cmd::CreateTexture(
            1,
            tex(
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                TextureFormat::Depth24PlusStencil8,
                texture_usage::RENDER_TARGET,
            ),
        ),
        Cmd::CreateBuffer(
            1,
            buf(
                quad(GREEN).len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::CreateBuffer(
            2,
            buf(
                fstri(GREEN).len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: quad(GREEN),
        },
        Cmd::WriteBuffer {
            id: 2,
            offset: 0,
            data: fstri(GREEN),
        },
    ];
    let mark_pipe = {
        next_pipe += 1;
        next_pipe
    };
    cmds.push(pipeline(mark_pipe, mark));
    let mark2_pipe = mark2.map(|m| {
        next_pipe += 1;
        cmds.push(pipeline(next_pipe, m));
        next_pipe
    });
    let test_pipe = {
        next_pipe += 1;
        next_pipe
    };
    cmds.push(pipeline(test_pipe, test));

    let mut enc = vec![
        // Pass A — MARK (clear stencil to 0).
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
        Enc::SetPipeline(mark_pipe),
        Enc::SetStencilReference { reference },
        Enc::SetVertexBuffer {
            slot: 0,
            buffer: mark_buffer,
            offset: 0,
        },
        Enc::Draw {
            vertex_count: mark_verts,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        },
        Enc::EndRenderPass,
    ];
    if let Some(p) = mark2_pipe {
        enc.extend([
            // Pass B — MARK AGAIN (LOAD stencil → accumulate).
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
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
            Enc::SetPipeline(p),
            Enc::SetStencilReference { reference },
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: mark_buffer,
                offset: 0,
            },
            Enc::Draw {
                vertex_count: mark_verts,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ]);
    }
    enc.extend([
        // Final pass — TEST (re-clear colour to blue, LOAD stencil, draw fullscreen green gated).
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: 1,
                load: LoadOp::Clear,
                clear: BLUE,
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
        Enc::SetPipeline(test_pipe),
        Enc::SetStencilReference { reference },
        Enc::SetVertexBuffer {
            slot: 0,
            buffer: 2,
            offset: 0,
        },
        Enc::Draw {
            vertex_count: 3,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        },
        Enc::EndRenderPass,
    ]);
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: enc,
        signal: None,
    }));
    run(&cmds)
}

#[test]
fn stencil_equal_gates_the_draw_to_the_marked_rect() {
    // MARK the rect (REPLACE reference 1 under ALWAYS), TEST the fullscreen draw EQUAL(1). Only the marked
    // rect passes → green there, blue elsewhere.
    let px = mark_then_test(
        1,
        1, // mark with the centred quad
        6,
        depth_stencil(compare::ALWAYS, stencil_op::REPLACE, 0xFF),
        None,
        depth_stencil(compare::EQUAL, stencil_op::KEEP, 0x00),
    );
    for y in 0..N {
        for x in 0..N {
            let want = if inside_rect(x, y) { GREEN_PX } else { BLUE_PX };
            assert_eq!(
                at(&px, x, y),
                want,
                "pixel ({x},{y}) inside_rect={} — stencil EQUAL must gate the draw",
                inside_rect(x, y)
            );
        }
    }
    let inside = ((3 * N / 4 - N / 4) * (3 * N / 4 - N / 4)) as usize; // 4x4 = 16 for N=8
    assert_eq!(
        count(&px, GREEN_PX),
        inside,
        "exactly the marked rect is green"
    );
    assert_eq!(
        count(&px, BLUE_PX),
        (N * N) as usize - inside,
        "the rest is the blue clear"
    );

    // CONTROL: the SAME geometry with the stencil test DISABLED (ALWAYS) floods the whole screen green —
    // proving the 16-vs-64 gap above is the stencil test, not the geometry.
    let control = mark_then_test(
        1,
        1,
        6,
        depth_stencil(compare::ALWAYS, stencil_op::REPLACE, 0xFF),
        None,
        depth_stencil(compare::ALWAYS, stencil_op::KEEP, 0x00),
    );
    assert_eq!(
        count(&control, GREEN_PX),
        (N * N) as usize,
        "stencil DISABLED (ALWAYS) → the fullscreen draw covers everything"
    );
}

#[test]
fn stencil_increment_accumulates_then_gates() {
    // TWO INCREMENT_CLAMP mark passes over the FULLSCREEN triangle (single triangle → no shared edge, so
    // each pixel is incremented exactly once per pass) drive every pixel's stencil 0→1→2. The fullscreen
    // TEST is EQUAL(2), so a pixel passes iff its stencil accumulated to exactly 2. If INCREMENT weren't
    // applied (or only one pass ran → stencil 1) EQUAL(2) fails everywhere → all blue; a full-green screen
    // proves INCREMENT was applied AND accumulated across the two passes.
    let px = mark_then_test(
        2,
        2, // mark with the fullscreen triangle (no shared edge → no double-count)
        3,
        depth_stencil(compare::ALWAYS, stencil_op::INCREMENT_CLAMP, 0xFF),
        Some(depth_stencil(
            compare::ALWAYS,
            stencil_op::INCREMENT_CLAMP,
            0xFF,
        )),
        depth_stencil(compare::EQUAL, stencil_op::KEEP, 0x00),
    );
    assert_eq!(
        count(&px, GREEN_PX),
        (N * N) as usize,
        "two INCREMENTs (0→1→2) then EQUAL(2) must pass everywhere; got {} green of {}",
        count(&px, GREEN_PX),
        N * N
    );

    // Discriminating control: a SINGLE increment leaves stencil at 1, so EQUAL(2) passes NOWHERE → all blue.
    let once = mark_then_test(
        2,
        2,
        3,
        depth_stencil(compare::ALWAYS, stencil_op::INCREMENT_CLAMP, 0xFF),
        None,
        depth_stencil(compare::EQUAL, stencil_op::KEEP, 0x00),
    );
    assert_eq!(
        count(&once, BLUE_PX),
        (N * N) as usize,
        "one INCREMENT → stencil 1, EQUAL(2) gates out everything"
    );
}
