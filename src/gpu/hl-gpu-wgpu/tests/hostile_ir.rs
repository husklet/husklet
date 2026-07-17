//! Adversarial / hostile-IR robustness sweep for the `WgpuExecutor`.
//!
//! The runtime's `validate` stage checks per-object *shape* (frame/buffer/texture ceilings, shader payload,
//! bind-group SET index, copy alignment) but does NOT bounds-check a submitted command's regions, ids, or
//! draw/dispatch counts (see `hl-gpu/src/runtime/service/validate.rs`). Every such check lands on the
//! backend. This suite mints deliberately MALFORMED IR — dangling ids, out-of-bounds regions, zero/huge
//! dimensions, mismatched formats, bad indices, count overflows — and asserts the executor's contract under
//! abuse:
//!
//!   1. the hostile submit returns a TYPED [`GpuError`] (or, where the op is defined to clamp, a clean
//!      partial no-op) and NEVER panics, and
//!   2. a known-good program run on the SAME executor immediately afterwards still produces the exact
//!      expected pixels — proving the abuse neither panicked the process nor lost/poisoned the device.
//!
//! This is the dedicated hostile counterpart to the oracle-vs-executor differential: wgpu 24's default
//! uncaptured-error handler PANICS on any validation error, so an unchecked hostile op is a hard crash, not
//! a soft failure. Each abuse below corresponds to a guard in `submit.rs` (bounds/overflow/format/type
//! checks) or the render/compute pass validation-scope net that converts a residual wgpu rejection into a
//! typed error. `catch_unwind` is used so a REGRESSION (a missing guard) is reported as a clear per-case
//! failure naming the abuse, rather than aborting the whole binary.

use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    ComputePipelineDesc, DepthAttachment, DepthState, Extent3d, Origin3d, RenderPipelineDesc,
    ShaderRef, TextureDesc, TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuError, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// -------------------------------------------------------------------------------------------------
// Shared device + harness (a test binary is one process; share one executor across the cases).
// -------------------------------------------------------------------------------------------------

static EXEC: OnceLock<Option<Mutex<WgpuExecutor>>> = OnceLock::new();

/// Lock the shared executor, or `None` if no adapter is reachable (skip, mirroring the rest of the suite).
fn exec() -> Option<MutexGuard<'static, WgpuExecutor>> {
    EXEC.get_or_init(|| {
        WgpuExecutor::new(DeviceConfig::default())
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
    .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()))
}

fn session(g: &WgpuExecutor) -> Session {
    let caps = g.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1; // byte-addressable copies (matches the rest of the wgpu suite)
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

const RT: u32 = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST;

fn tex(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
}

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.to_string(),
        source: source.to_string(),
    }
    .to_words()
}

/// The canonical GOOD program: create a fresh 2x2 `Rgba8Unorm` render target, `ClearRect`-fill it with a
/// known texel, read it back, and assert every pixel is exact. If this succeeds AFTER a hostile submit, the
/// executor survived the abuse (no panic, no device loss). Uses fresh ids/session so nothing leaks between
/// cases. Wrapped in `catch_unwind` so a device-lost survivor is a clear failure, not an abort.
fn assert_survives(g: &mut WgpuExecutor, label: &str) {
    const TEXEL: [u8; 4] = [17, 34, 51, 255];
    let mut s = session(g);
    let ran = std::panic::catch_unwind(AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(
            &mut s,
            g,
            0,
            &[
                Cmd::CreateTexture(1000, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::ClearRect {
                        texture: 1000,
                        x: 0,
                        y: 0,
                        w: 2,
                        h: 2,
                        color: [
                            TEXEL[0] as f32 / 255.0,
                            TEXEL[1] as f32 / 255.0,
                            TEXEL[2] as f32 / 255.0,
                            1.0,
                        ],
                    }],
                    signal: None,
                }),
            ],
        )
    }));
    match ran {
        Err(_) => panic!(
            "[{label}] the good program PANICKED after the abuse — the executor did not survive"
        ),
        Ok(Err(e)) => panic!(
            "[{label}] the good program failed ({e:?}) after the abuse — device lost/poisoned"
        ),
        Ok(Ok(_)) => {}
    }
    let px = g
        .read_texture(&s.resources, 1000)
        .expect("read back the good render target");
    for (i, out) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            out, TEXEL,
            "[{label}] good pixel {i} wrong — executor state corrupt after the abuse"
        );
    }
}

/// Assert one hostile program is a TYPED error (never a panic, never fake `Ok`) whose variant matches
/// `want`, then prove the executor survives with the good program. This is the core hostile contract.
fn hostile(g: &mut WgpuExecutor, label: &str, cmds: &[Cmd], want: impl Fn(&GpuError) -> bool) {
    let mut s = session(g);
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(&mut s, g, 0, cmds)
    }));
    match r {
        Err(_) => {
            panic!("[{label}] PANICKED — hostile IR must return a typed GpuError, never panic")
        }
        Ok(Ok(_)) => {
            panic!("[{label}] returned Ok — hostile IR must be a typed GpuError, not fake success")
        }
        Ok(Err(e)) => assert!(want(&e), "[{label}] wrong error variant: got {e:?}"),
    }
    drop(s);
    assert_survives(g, label);
}

fn is_oob(e: &GpuError) -> bool {
    matches!(e, GpuError::OutOfBounds)
}
fn is_unknown(e: &GpuError) -> bool {
    matches!(e, GpuError::UnknownId { .. })
}
fn is_invalid(e: &GpuError) -> bool {
    matches!(e, GpuError::Invalid(_))
}
/// Some abuses are rejected by an upstream stage (validate) as a `ResourceLimit`, others by the backend —
/// accept either "structurally rejected" shape.
fn is_rejected(e: &GpuError) -> bool {
    matches!(
        e,
        GpuError::Invalid(_) | GpuError::ResourceLimit(_) | GpuError::OutOfBounds
    )
}

// A trivial bindingless triangle pipeline (id `pid`) writing solid white — for draw-path abuses.
fn white_triangle_pipeline(pid: u32, vs_id: u32, fs_id: u32) -> Vec<Cmd> {
    let vs = "#version 460\nvoid main(){ gl_Position = vec4(0.0,0.0,0.0,1.0); }\n";
    let fs = "#version 460\nlayout(location=0) out vec4 c; void main(){ c = vec4(1.0); }\n";
    vec![
        Cmd::CreateShader {
            id: vs_id,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
        },
        Cmd::CreateShader {
            id: fs_id,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
        },
        Cmd::CreateRenderPipeline(
            pid,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: vs_id,
                    entry: "vmain".into(),
                },
                fragment: Some(ShaderRef {
                    module: fs_id,
                    entry: "fmain".into(),
                }),
                vertex_buffers: vec![],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        ),
    ]
}

const COMPUTE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(1) fn main() { data[0] = data[0] + 1u; }
"#;

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

/// A valid compute pipeline (id 10) + storage buffer (id 10) + bind group (id 10) — for dispatch abuses.
fn compute_setup() -> Vec<Cmd> {
    vec![
        Cmd::CreateShader {
            id: 10,
            kind: ShaderPayloadKind::SpirV,
            spirv: wgsl_to_spirv(COMPUTE_WGSL),
        },
        Cmd::CreateComputePipeline(
            10,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 10,
                    entry: "main".into(),
                },
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            10,
            buf(
                16,
                buffer_usage::STORAGE | buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::CreateBindGroup(
            10,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::Buffer {
                        id: 10,
                        offset: 0,
                        size: 16,
                    },
                }],
            },
        ),
    ]
}

fn sub() -> TextureSubresource {
    TextureSubresource::base()
}

// =================================================================================================
// (1) DANGLING / never-created ids
// =================================================================================================

#[test]
fn dangling_buffer_in_copy_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_buffer_copy",
        &[
            Cmd::CreateBuffer(1, buf(64, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            // src 999 was never created.
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 999,
                    src_offset: 0,
                    dst: 1,
                    dst_offset: 0,
                    size: 16,
                }],
                signal: None,
            }),
        ],
        is_unknown,
    );
}

#[test]
fn dangling_texture_in_copy_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_texture_copy",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToTexture {
                    src: 777,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 1,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_unknown,
    );
}

#[test]
fn dangling_pipeline_in_draw_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_pipeline_draw",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
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
                    Enc::SetPipeline(555), // never created
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
        is_unknown,
    );
}

#[test]
fn dangling_vertex_buffer_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::Submit(CommandBuffer {
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
                buffer: 404,
                offset: 0,
            }, // never created
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "dangling_vertex_buffer", &cmds, is_unknown);
}

#[test]
fn dangling_pipeline_in_dispatch_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_dispatch_pipeline",
        &[
            Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE)),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 16,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(321), // never created
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: 1, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
        is_unknown,
    );
}

// =================================================================================================
// (2) OUT-OF-BOUNDS regions
// =================================================================================================

#[test]
fn copy_buffer_to_buffer_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2b_overhang",
        &[
            Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 1,
                    src_offset: 0,
                    dst: 2,
                    dst_offset: 0,
                    size: 64,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_buffer_to_texture_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2t_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(
                1,
                buf(65536, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 0,
                    dst: 1,
                    mip: 0,
                    width: 64,
                    height: 64,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_buffer_to_texture_bad_mip_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2t_bad_mip",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(
                1,
                buf(4096, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 0,
                    dst: 1,
                    mip: 9,
                    width: 4,
                    height: 4,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_texture_to_texture_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2t2t_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d { x: 3, y: 3, z: 0 },
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    }, // 3+4 > 4
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_texture_to_buffer_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "t2b_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(
                1,
                buf(65536, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToBuffer {
                    src: 1,
                    mip: 0,
                    width: 64,
                    height: 64,
                    dst: 1,
                    dst_offset: 0,
                    bytes_per_row: 0,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn fill_buffer_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "fill_overhang",
        &[
            Cmd::CreateBuffer(1, buf(64, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 60,
                    size: 32,
                    value: 0xdead_beef,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn fill_buffer_offset_size_overflow_is_oob() {
    let Some(mut g) = exec() else { return };
    // offset + size overflows u64: without a guard this is a debug arithmetic PANIC.
    hostile(
        &mut g,
        "fill_overflow",
        &[
            Cmd::CreateBuffer(1, buf(64, buffer_usage::COPY_DST)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: u64::MAX - 2,
                    size: 8,
                    value: 0xff,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn blit_into_smaller_target_is_oob() {
    let Some(mut g) = exec() else { return };
    // dst rect (0,0 .. 8x8) overhangs a 4x4 destination.
    hostile(
        &mut g,
        "blit_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 8,
                        height: 8,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

/// `ClearRect` is DEFINED to clamp an over-hanging rect to the covered sub-rectangle (matching the CPU
/// oracle), NOT to error. Assert the clamp: an over-hang fills only the in-bounds texels and leaves the
/// rest at the pre-clear value — and of course does not panic.
#[test]
fn clear_rect_overhang_clamps_not_errors() {
    let Some(mut g) = exec() else { return };
    let mut s = session(&g);
    // Pre-clear the whole 4x4 to black, then a red ClearRect at (2,2) size 8x8 that overhangs to the edge:
    // it must fill ONLY the 2x2 bottom-right corner, leaving the rest black.
    hl_gpu::runtime::submit(
        &mut s,
        &mut *g,
        0,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::ClearRect {
                        texture: 1,
                        x: 0,
                        y: 0,
                        w: 4,
                        h: 4,
                        color: [0.0, 0.0, 0.0, 1.0],
                    },
                    Enc::ClearRect {
                        texture: 1,
                        x: 2,
                        y: 2,
                        w: 8,
                        h: 8,
                        color: [1.0, 0.0, 0.0, 1.0],
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("an over-hanging ClearRect must clamp (a defined no-op past the edge), never error");
    let px = g.read_texture(&s.resources, 1).unwrap();
    for y in 0..4u32 {
        for x in 0..4u32 {
            let o = ((y * 4 + x) * 4) as usize;
            let got = [px[o], px[o + 1], px[o + 2], px[o + 3]];
            let want = if x >= 2 && y >= 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(got, want, "clamped ClearRect pixel ({x},{y})");
        }
    }
    drop(s);
    assert_survives(&mut g, "clear_rect_overhang_clamps");
}

// =================================================================================================
// (3) ZERO-SIZE / absurdly-huge dimensions
// =================================================================================================

#[test]
fn zero_width_texture_is_rejected() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "zero_texture",
        &[Cmd::CreateTexture(
            1,
            tex(0, 4, TextureFormat::Rgba8Unorm, RT),
        )],
        is_rejected,
    );
}

#[test]
fn huge_texture_is_rejected() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "huge_texture",
        &[Cmd::CreateTexture(
            1,
            tex(100_000, 100_000, TextureFormat::Rgba8Unorm, RT),
        )],
        is_rejected,
    );
}

#[test]
fn oversized_dispatch_is_oob() {
    let Some(mut g) = exec() else { return };
    let mut cmds = compute_setup();
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(10),
            Enc::SetBindGroup {
                index: 0,
                group: 10,
            },
            Enc::Dispatch {
                x: 4_000_000,
                y: 1,
                z: 1,
            },
            Enc::EndComputePass,
        ],
        signal: None,
    }));
    hostile(&mut g, "oversized_dispatch", &cmds, is_oob);
}

#[test]
fn zero_size_blit_is_invalid() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "zero_blit",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 0,
                        height: 4,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

// =================================================================================================
// (4) MISMATCHED formats
// =================================================================================================

#[test]
fn copy_texture_to_texture_between_incompatible_formats_converts_not_rejects() {
    let Some(mut g) = exec() else { return };
    // R8 (1 byte/texel) → Rgba8 (4 bytes/texel): DIFFERENT texel layouts. GL permits this as a CONVERTING
    // copy (the red channel expands to (R,0,0,1)); the executor now routes a format mismatch through a
    // converting blit instead of rejecting it (previously `Invalid("… incompatible formats")`). Prove it
    // SUCCEEDS and leaves the executor healthy — the exact-conversion pixel checks live in `t2t_convert.rs`.
    let mut s = session(&g);
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(
            &mut s,
            &mut *g,
            0,
            &[
                Cmd::CreateTexture(1, tex(4, 4, TextureFormat::R8Unorm, RT)),
                Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyTextureToTexture {
                        src: 1,
                        src_sub: sub(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: sub(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    }],
                    signal: None,
                }),
            ],
        )
    }));
    match r {
        Err(_) => panic!("[c2t2t_convert] converting copy PANICKED"),
        Ok(Err(e)) => panic!("[c2t2t_convert] converting copy must succeed, got {e:?}"),
        Ok(Ok(_)) => {}
    }
    drop(s);
    assert_survives(&mut g, "c2t2t_convert");
}

#[test]
fn resolve_non_multisampled_source_is_invalid() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "resolve_non_msaa",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)), // single-sampled src
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

#[test]
fn resolve_format_mismatch_is_invalid() {
    let Some(mut g) = exec() else { return };
    // Multisampled src, single-sample dst, but different formats.
    let msaa = TextureDesc {
        sample_count: 4,
        ..tex(
            4,
            4,
            TextureFormat::Rgba8Unorm,
            texture_usage::RENDER_TARGET,
        )
    };
    hostile(
        &mut g,
        "resolve_fmt_mismatch",
        &[
            Cmd::CreateTexture(1, msaa),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Bgra8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

// =================================================================================================
// (5) DEPTH / attachment mismatches
// =================================================================================================

#[test]
fn depth_attachment_on_color_format_is_invalid() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "depth_attach_color",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)), // color, misused as depth
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: Some(DepthAttachment {
                            texture: 2,
                            load: LoadOp::Clear,
                            clear_depth: 1.0,
                            clear_stencil: 0,
                        }),
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

#[test]
fn depth_tested_pipeline_in_color_only_pass_is_invalid() {
    let Some(mut g) = exec() else { return };
    let vs = "#version 460\nvoid main(){ gl_Position = vec4(0.0,0.0,0.5,1.0); }\n";
    let fs = "#version 460\nlayout(location=0) out vec4 c; void main(){ c = vec4(1.0); }\n";
    hostile(
        &mut g,
        "depth_pipe_color_pass",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
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
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: Some(DepthState::depth_only(
                        TextureFormat::Depth32Float,
                        true,
                        compare::LESS,
                    )),
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // pipeline wants a depth attachment; the pass has none.
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
        is_invalid,
    );
}

// =================================================================================================
// (6) BAD indices + count overflows
// =================================================================================================

#[test]
fn bind_group_index_out_of_range_is_invalid() {
    let Some(mut g) = exec() else { return };
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::CreateBuffer(1, buf(16, buffer_usage::UNIFORM)));
    cmds.push(Cmd::CreateBindGroup(
        1,
        BindGroupDesc {
            set: 0,
            entries: vec![BindEntry {
                binding: 0,
                resource: BindResource::Buffer {
                    id: 1,
                    offset: 0,
                    size: 16,
                },
            }],
        },
    ));
    cmds.push(Cmd::Submit(CommandBuffer {
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
            Enc::SetBindGroup { index: 7, group: 1 }, // >= max_bind_groups (4)
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "bind_index_oor", &cmds, is_invalid);
}

#[test]
fn vertex_buffer_offset_beyond_buffer_is_oob() {
    let Some(mut g) = exec() else { return };
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::CreateBuffer(
        1,
        buf(32, buffer_usage::VERTEX | buffer_usage::COPY_DST),
    ));
    cmds.push(Cmd::Submit(CommandBuffer {
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
                offset: 4096,
            }, // past the 32-byte buffer -> would panic slice()
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "vbuf_bad_offset", &cmds, is_oob);
}

#[test]
fn draw_range_overflow_is_invalid() {
    let Some(mut g) = exec() else { return };
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::Submit(CommandBuffer {
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
            // first_vertex + vertex_count overflows u32 -> would panic building the draw range.
            Enc::Draw {
                vertex_count: 100,
                instance_count: 1,
                first_vertex: u32::MAX - 10,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "draw_overflow", &cmds, is_invalid);
}

#[test]
fn draw_vertex_count_beyond_bound_buffer_is_invalid() {
    let Some(mut g) = exec() else { return };
    // A pipeline that reads a per-vertex attribute, a tiny (24-byte) vertex buffer, and a draw of 100000
    // vertices — wgpu rejects the overrun at pass-end; the validation-scope net makes it a typed error.
    let vs = "#version 460\nlayout(location=0) in vec2 p; void main(){ gl_Position = vec4(p,0.0,1.0); }\n";
    let fs = "#version 460\nlayout(location=0) out vec4 c; void main(){ c = vec4(1.0); }\n";
    hostile(
        &mut g,
        "draw_beyond_vbuf",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(1, buf(24, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
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
                            format: 2,
                            offset: 0,
                        }],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
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
                        vertex_count: 100_000,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

// =================================================================================================
// (7) STENCIL on a non-stencil target — SetStencilReference is a harmless no-op (clamp), not a crash
// =================================================================================================

#[test]
fn set_stencil_reference_without_stencil_is_harmless_noop() {
    let Some(mut g) = exec() else { return };
    // A plain color pipeline (no depth/stencil) drawn in a color-only pass, with a SetStencilReference in
    // the stream: the reference has no stencil to test against, so it is a defined no-op — the draw still
    // runs and paints the target white. Proves a stray stencil-state op neither errors spuriously nor panics.
    let mut s = session(&g);
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(2, 2, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    // A fullscreen triangle so the 2x2 target is fully covered.
    let vs = "#version 460\nvoid main(){ vec2 p[3] = vec2[3](vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0)); gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0); }\n";
    cmds[1] = Cmd::CreateShader {
        id: 1,
        kind: ShaderPayloadKind::Glsl,
        spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
    };
    cmds.push(Cmd::Submit(CommandBuffer {
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
            Enc::SetStencilReference { reference: 0x7f }, // no stencil aspect -> harmless
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hl_gpu::runtime::submit(&mut s, &mut *g, 0, &cmds).expect(
        "SetStencilReference on a non-stencil target must be a harmless no-op, not an error/panic",
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    for (i, out) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            out,
            [255, 255, 255, 255],
            "pixel {i}: the draw must still paint white despite the stray stencil-ref"
        );
    }
    drop(s);
    assert_survives(&mut g, "stray_stencil_ref");
}
