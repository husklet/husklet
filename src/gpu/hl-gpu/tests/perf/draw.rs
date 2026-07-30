use super::*;

use hl_gpu::protocol::model::descriptor::{BindEntry, BindResource, ColorAttachment};
use hl_gpu::protocol::model::enums::LoadOp;

/// Draws in one frame. Chosen so the frame's encoder carries ~1800 ops — the shape a `glmark2 -b ideas`
/// frame showed (1782 commands per submit at 358 ms/frame, i.e. ~200 µs/command; see the wgpu executor's
/// `tests/frame_profile.rs` for where that time actually goes).
const DRAWS: usize = 600;

/// A frame in the shape `hl-gl` lowers a GL draw stream to today: per draw a fresh `UNIFORM` buffer, its
/// write, and a fresh bind group; then one `Submit` whose render pass sets the bind group and draws; then
/// the destroys that return the frame's ephemeral resources.
fn draw_frame() -> Vec<Cmd> {
    let mut cmds = Vec::with_capacity(DRAWS * 5 + 1);
    let mut ops = Vec::with_capacity(DRAWS * 3 + 3);
    ops.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: 1,
            load: LoadOp::Clear,
            clear: [0.0, 0.0, 0.0, 1.0],
            store: true,
        }],
        depth: None,
    });
    ops.push(Enc::SetPipeline(1));
    ops.push(Enc::SetVertexBuffer {
        slot: 0,
        buffer: 1,
        offset: 0,
    });
    for i in 0..DRAWS {
        let id = 16 + i as u32;
        cmds.push(Cmd::CreateBuffer(
            id,
            BufferDesc {
                size: 64,
                usage: buffer_usage::UNIFORM,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id,
            offset: 0,
            data: vec![0u8; 64],
        });
        cmds.push(Cmd::CreateBindGroup(
            id,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::Buffer {
                        id,
                        offset: 0,
                        size: 64,
                    },
                }],
            },
        ));
        ops.push(Enc::SetBindGroup {
            index: 0,
            group: id,
        });
        ops.push(Enc::Draw {
            vertex_count: 4,
            instance_count: 1,
            first_vertex: (i * 4) as u32,
            first_instance: 0,
        });
    }
    ops.push(Enc::EndRenderPass);
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: ops,
        signal: None,
    }));
    for i in 0..DRAWS {
        let id = 16 + i as u32;
        cmds.push(Cmd::DestroyBindGroup(id));
        cmds.push(Cmd::DestroyBuffer(id));
    }
    cmds
}

/// Time the three PURE-CPU stages a draw-heavy frame pays before the executor sees it: codec encode,
/// codec decode, and `validate`. Accounting needs a live `Session`, so it is measured in the wgpu crate's
/// `frame_profile` alongside executor dispatch; this test guards the codec/validate half, which needs no
/// GPU and therefore runs everywhere.
///
/// Measured 2026-07-30 on aarch64 Linux (release): encode ~0.03 µs/cmd, decode ~0.03 µs/cmd, validate
/// ~0.002 µs/cmd — together under 0.3 ms for a 600-draw frame. The per-draw cost a slow frame is made of
/// is NOT here.
#[test]
fn perf_draw_frame_cpu_stages() {
    let cmds = draw_frame();
    let commands = cmds.len();
    let wire = hl_gpu::Encoder::stream(&cmds);
    let limits = Limits::from_capabilities(Capabilities::permissive_fixture("host"));

    for _ in 0..3 {
        let e = hl_gpu::Encoder::stream(&cmds);
        let d = hl_gpu::Decoder::stream(&e).unwrap();
        hl_gpu::runtime::service::validate::validate(&limits, e.len(), &d).unwrap();
    }

    let iters = 20u32;
    let t = Instant::now();
    for _ in 0..iters {
        let _ = hl_gpu::Encoder::stream(&cmds);
    }
    let encode = t.elapsed() / iters;

    let t = Instant::now();
    for _ in 0..iters {
        let _ = hl_gpu::Decoder::stream(&wire).unwrap();
    }
    let decode = t.elapsed() / iters;

    let decoded = hl_gpu::Decoder::stream(&wire).unwrap();
    let t = Instant::now();
    for _ in 0..iters {
        hl_gpu::runtime::service::validate::validate(&limits, wire.len(), &decoded).unwrap();
    }
    let validate = t.elapsed() / iters;

    println!(
        "perf: draw frame ({DRAWS} draws, {commands} cmds, {} wire bytes)",
        wire.len()
    );
    println!(
        "perf:   encode   {:8.1} us/frame  {:6.4} us/cmd",
        us(encode),
        us(encode) / commands as f64
    );
    println!(
        "perf:   decode   {:8.1} us/frame  {:6.4} us/cmd",
        us(decode),
        us(decode) / commands as f64
    );
    println!(
        "perf:   validate {:8.1} us/frame  {:6.4} us/cmd",
        us(validate),
        us(validate) / commands as f64
    );

    // Loose ceiling: a 600-draw frame's CPU-side codec + validate must stay far below one 60 Hz frame
    // (16.6 ms). Trips on an order-of-magnitude regression, not on a busy box.
    let total = us(encode) + us(decode) + us(validate);
    assert!(
        total < 8000.0,
        "codec+validate for a {DRAWS}-draw frame regressed: {total:.0} us"
    );
}
