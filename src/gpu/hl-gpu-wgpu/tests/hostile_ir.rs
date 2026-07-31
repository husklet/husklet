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

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

/// Lock the shared executor. A missing adapter is a hard failure, not a skip.
fn exec() -> MutexGuard<'static, WgpuExecutor> {
    EXEC.get_or_init(|| {
        Mutex::new(
            WgpuExecutor::new(DeviceConfig::default())
                .expect("a GPU adapter is required to prove the wgpu executor"),
        )
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
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
                        base_array_layer: 0,
                        layer_count: 1,
                        mip_level: 0,
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
    matches!(e, GpuError::Invalid(_) | GpuError::Kernel(_))
}
/// Some abuses are rejected by an upstream stage (validate) as a `ResourceLimit`, others by the backend —
/// accept either "structurally rejected" shape.
fn is_rejected(e: &GpuError) -> bool {
    matches!(
        e,
        GpuError::Invalid(_)
            | GpuError::Kernel(_)
            | GpuError::ResourceLimit(_)
            | GpuError::OutOfBounds
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

#[path = "hostile_ir/bounds.rs"]
mod bounds;
#[path = "hostile_ir/depth.rs"]
mod depth;
#[path = "hostile_ir/dimensions.rs"]
mod dimensions;
#[path = "hostile_ir/draw.rs"]
mod draw;
#[path = "hostile_ir/format.rs"]
mod format;
#[path = "hostile_ir/resource.rs"]
mod resource;
#[path = "hostile_ir/stencil.rs"]
mod stencil;
