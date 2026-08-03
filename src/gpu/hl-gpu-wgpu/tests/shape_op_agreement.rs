//! For every texture SHAPE crossed with every transfer OP, the executor and the software reference must
//! agree on whether the operation is legal.
//!
//! Not on the pixels — on the ANSWER. Whether a program is refused is as much a part of a backend's
//! behaviour as what it computes, and it is the half no pixel comparison can reach: a program one side
//! refuses never produces bytes to compare, so the differential silently drops it and reports clean. Every
//! shape outside plain 2D lived in that gap, and the gap is where a mirrored blit, a layered clear and a
//! float target each hid.
//!
//! This found a real divergence on its first run, in code committed two hours earlier. A
//! `CopyBufferToTexture` into a multi-plane destination is issued by the executor over the texture's full
//! layer/slice/face count, so it refuses a buffer that cannot supply every plane; the reference wrote the
//! base plane and reported success. The REFERENCE accepting what the SUBJECT refuses is the direction that
//! makes a differential agree by one side being wrong, and it was introduced by the commit that taught the
//! reference layered storage — the same commit that added three explicit refusals precisely to avoid it.
//! Three refusals were found by looking; the fourth needed this matrix.
//!
//! What this test does NOT establish, stated so nobody reads more into it: agreement here is agreement
//! about legality, plus whatever the base-plane readback can see. Neither backend can read back a non-base
//! layer, slice or face through a channel the other also serves, so the CONTENT of a non-base plane is
//! unverified on both sides. Closing that needs the region buffer↔texture copies, which the reference
//! refuses as a whole op class — recorded in `oracle_spec::layered` with its own condition.
mod gpu_harness;
use gpu_harness::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::{
    Cmd, CommandBuffer, CpuExecutor, Enc, FakeClock, GlobalLedger, GpuError, GpuExecutor, Limits,
    Session,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn d(dim: TextureDim, w: u32, h: u32, depth: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth,
        mip_levels: 1,
        sample_count: 1,
        dim,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST,
        label: String::new(),
    }
}
fn sb() -> TextureSubresource {
    TextureSubresource::base()
}
fn sub1() -> TextureSubresource {
    TextureSubresource {
        mip: 0,
        layer: 1,
        aspect: TextureAspect::All,
    }
}

fn programs(desc: &TextureDesc) -> Vec<(&'static str, Vec<Cmd>)> {
    let (w, h) = (desc.width, desc.height);
    let ext = Extent3d {
        width: w,
        height: h,
        depth: 1,
    };
    let pre = |extra: Vec<Enc>| {
        vec![
            Cmd::CreateTexture(1, desc.clone()),
            Cmd::CreateTexture(2, d(TextureDim::D2, w, h, 1)),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: (w * h * 4) as u64,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: extra,
                signal: None,
            }),
        ]
    };
    vec![
        (
            "ClearRect",
            pre(vec![Enc::ClearRect {
                texture: 1,
                x: 0,
                y: 0,
                w,
                h,
                color: [1.0, 0.0, 0.0, 1.0],
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 0,
            }]),
        ),
        (
            "B2T",
            pre(vec![Enc::CopyBufferToTexture {
                src: 1,
                src_offset: 0,
                bytes_per_row: w * 4,
                dst: 1,
                mip: 0,
                width: w,
                height: h,
            }]),
        ),
        (
            "T2B",
            pre(vec![Enc::CopyTextureToBuffer {
                src: 1,
                mip: 0,
                width: w,
                height: h,
                dst: 1,
                dst_offset: 0,
                bytes_per_row: w * 4,
            }]),
        ),
        (
            "T2T",
            pre(vec![Enc::CopyTextureToTexture {
                src: 1,
                src_sub: sb(),
                src_origin: Origin3d::default(),
                dst: 2,
                dst_sub: sb(),
                dst_origin: Origin3d::default(),
                extent: ext,
            }]),
        ),
        (
            "Blit",
            pre(vec![Enc::BlitTexture {
                src: 1,
                src_sub: sb(),
                src_origin: Origin3d::default(),
                src_extent: ext,
                dst: 2,
                dst_sub: sb(),
                dst_origin: Origin3d::default(),
                dst_extent: ext,
                filter: Filter::Nearest,
                mirror: Mirror::NONE,
            }]),
        ),
        // The REGION copies at a NON-BASE layer, which is the only channel that observes a plane the
        // whole-texture readback cannot reach. On a single-plane shape layer 1 does not exist and both
        // backends must say so — agreement about the bound is as much the subject as agreement about
        // the transfer.
        (
            "B2Trgn",
            pre(vec![Enc::CopyBufferToTextureRegion {
                src: 1,
                src_offset: 0,
                bytes_per_row: w * 4,
                rows_per_image: h,
                dst: 1,
                dst_sub: sub1(),
                dst_origin: Origin3d::default(),
                extent: ext,
            }]),
        ),
        (
            "T2Brgn",
            pre(vec![Enc::CopyTextureToBufferRegion {
                src: 1,
                src_sub: sub1(),
                src_origin: Origin3d::default(),
                extent: ext,
                dst: 1,
                dst_offset: 0,
                bytes_per_row: w * 4,
                rows_per_image: h,
            }]),
        ),
        (
            "Pass",
            pre(vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [0.0, 0.0, 0.0, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ]),
        ),
    ]
}

/// Did the backend ANSWER — ran, or refused with a typed reason it chose — as opposed to crashing or
/// letting the graphics API answer for it?
///
/// A boolean `is_ok()` cannot tell a clean refusal from a panic, and the two are not interchangeable. The
/// control for this very test proved it: reverting the reference's bounds check left the matrix green,
/// because the unchecked slice then panicked and a caught panic reads as a refusal. A crash agreeing with
/// a refusal is not agreement.
fn answered(result: hl_gpu::Result<()>, shape: &str, op: &str, side: &str) -> bool {
    match result {
        Ok(()) => true,
        Err(GpuError::Panicked(m)) => panic!(
            "{side} PANICKED on {shape}/{op}: {m}\n\nA panic is not a refusal. Whatever the other \
             backend does, a bounds or slice error escaping into the runtime is a defect on this side."
        ),
        Err(GpuError::Kernel(m)) => panic!(
            "{side} let the graphics API answer for it on {shape}/{op}: {m}\n\nA device-validation \
             error names the API call that failed rather than what the caller did; refuse it here."
        ),
        Err(_) => false,
    }
}

#[test]
fn every_shape_and_op_agrees_on_what_is_legal() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("adapter");
    let mut mismatches = 0;
    let mut compared = 0usize;
    let mut accepted = 0usize;
    for (name, desc) in [
        ("1D", d(TextureDim::D1, 4, 1, 1)),
        ("3D", d(TextureDim::D3, 2, 2, 3)),
        ("Cube", d(TextureDim::Cube, 2, 2, 6)),
        ("2Darr", d(TextureDim::D2, 2, 2, 3)),
        ("2D", d(TextureDim::D2, 2, 2, 1)),
        // A MULTI-LEVEL texture. Every shape above declares one mip level, which is why this matrix
        // did not reach the level axis at all: a three-level texture named as a colour attachment
        // reached `RenderPass::end` as `TextureViewIsNotRenderable { reason: MipLevelCount(3) }`, and
        // nothing here could see it because nothing here had more than one level.
        (
            "2Dmip",
            TextureDesc {
                mip_levels: 3,
                ..d(TextureDim::D2, 4, 4, 1)
            },
        ),
    ] {
        let mut line = format!("{name:6}");
        for (op, cmds) in programs(&desc) {
            let mut s = new_session(&exec);
            let host = answered(
                hl_gpu::runtime::submit(&mut s, &mut exec, 0, &cmds).map(|_| ()),
                name,
                op,
                "executor",
            );
            let mut cpu = CpuExecutor::new();
            let mut s2 = Session::new(
                Limits::from_capabilities(cpu.capabilities()),
                GlobalLedger::unbounded(),
                Box::new(FakeClock::new(0)),
            );
            let refr = answered(
                hl_gpu::runtime::submit(&mut s2, &mut cpu, 0, &cmds).map(|_| ()),
                name,
                op,
                "reference",
            );
            compared += 1;
            if host {
                accepted += 1;
            }
            let mark = if host == refr {
                if host {
                    "ok "
                } else {
                    "REF"
                }
            } else {
                mismatches += 1;
                let mut s3 = new_session(&exec);
                let he = hl_gpu::runtime::submit(&mut s3, &mut exec, 0, &cmds)
                    .err()
                    .map(|e| format!("{e:?}").chars().take(90).collect::<String>());
                let mut cpu2 = CpuExecutor::new();
                let mut s4 = Session::new(
                    Limits::from_capabilities(cpu2.capabilities()),
                    GlobalLedger::unbounded(),
                    Box::new(FakeClock::new(0)),
                );
                let re = hl_gpu::runtime::submit(&mut s4, &mut cpu2, 0, &cmds)
                    .err()
                    .map(|e| format!("{e:?}").chars().take(70).collect::<String>());
                eprintln!("  MISMATCH {name}/{op}: host_ok={host} ref_ok={refr}\n    host_err={he:?}\n    ref_err={re:?}");
                "!!!"
            };
            line.push_str(&format!(" {op}={mark}"));
        }
        eprintln!("AGREE {line}");
    }

    // What would this print if it were measuring nothing? Without these, a run in which every program
    // failed for an unrelated reason — or in which both backends refused everything — would be
    // indistinguishable from perfect agreement, because refusing in unison IS agreement here.
    assert_eq!(
        compared, 48,
        "the shape x op matrix must be fully exercised; {compared} of 48 cells ran"
    );
    assert!(
        accepted >= 24,
        "only {accepted} of {compared} cells were ACCEPTED by the executor — agreement made mostly of \
         mutual refusal proves little, and this matrix has lost its power"
    );
    assert_eq!(
        mismatches, 0,
        "the executor and the reference disagree about what is LEGAL in {mismatches} cell(s); see the \
         MISMATCH lines above. Whichever side is wrong, this is a divergence the pixel differential \
         cannot see: a program one backend refuses never produces bytes to compare."
    );
}
