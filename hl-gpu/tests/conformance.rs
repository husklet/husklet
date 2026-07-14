//! Executor-neutral GPU conformance suite — the semantic oracle for the hl-gpu IR.
//!
//! Every test drives an IR program through a [`GpuBackend`] and asserts the exact observable result
//! (buffer readback bytes, texture pixel readback). The backend under test is chosen behind the single
//! [`make_backend`] seam, which returns the [`SoftwareBackend`] reference executor today. When a real
//! host executor lands (`hl-gpu-wgpu`'s `WgpuBackend`), pointing `make_backend` at it must reproduce
//! byte-for-byte the same results — that is the whole point of freezing behavior here before the
//! boundary-first refactor.
//!
//! Programs are expressed as `Vec<Cmd>` and replayed via [`hl_gpu::replay::replay`], exactly the path
//! the host uses, so this exercises the real IR → backend seam and not a private shortcut.

use hl_gpu::backend::GpuBackend;
use hl_gpu::id::*;
use hl_gpu::ir::*;
use hl_gpu::ptx::{KernelDescriptor, KERNEL_MAGIC, VECADD_PTX};
use hl_gpu::replay;
use hl_gpu::software::SoftwareBackend;

/// The single seam that selects the executor under conformance test. Returns the reference software
/// backend today; a future `WgpuBackend` (or `CudaBackend`) must pass this identical suite unchanged.
fn make_backend() -> impl GpuBackend {
    SoftwareBackend::new()
}

// -------------------------------------------------------------------------------------------------
// small IR construction helpers (shared shape across cases)
// -------------------------------------------------------------------------------------------------

fn buffer(size: u64, usage: u32) -> BufferDesc {
    BufferDesc { size, usage, label: String::new() }
}

fn texture(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
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

/// Replay a whole program against a fresh backend, panicking on any replay error (a conformance
/// program is expected to be well-formed).
fn run(cmds: &[Cmd]) -> impl GpuBackend {
    let mut be = make_backend();
    replay::replay(&mut be, cmds).expect("conformance program must replay cleanly");
    be
}

// -------------------------------------------------------------------------------------------------
// buffer: write + readback
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_write_then_readback_exact_bytes() {
    let data = vec![0x01u8, 0x02, 0x03, 0x04, 0xAA, 0xBB, 0xCC, 0xDD];
    let mut be = run(&[
        Cmd::CreateBuffer(1, buffer(8, ir_buffer_usage::COPY_DST | ir_buffer_usage::COPY_SRC)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: data.clone() },
    ]);
    let mut out = [0u8; 8];
    be.read_buffer(BufferId(1), 0, &mut out).unwrap();
    assert_eq!(out, data.as_slice());
}

#[test]
fn buffer_write_at_offset_leaves_prefix_zeroed() {
    // A fresh buffer is zero-initialized; a write at a non-zero offset must touch only that span.
    let mut be = run(&[
        Cmd::CreateBuffer(1, buffer(8, ir_buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 4, data: vec![0x11, 0x22, 0x33, 0x44] },
    ]);
    let mut out = [0u8; 8];
    be.read_buffer(BufferId(1), 0, &mut out).unwrap();
    assert_eq!(out, [0, 0, 0, 0, 0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn buffer_partial_readback_window() {
    let mut be = run(&[
        Cmd::CreateBuffer(1, buffer(8, ir_buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0, 1, 2, 3, 4, 5, 6, 7] },
    ]);
    let mut out = [0u8; 3];
    be.read_buffer(BufferId(1), 2, &mut out).unwrap();
    assert_eq!(out, [2, 3, 4]);
}

// -------------------------------------------------------------------------------------------------
// buffer -> buffer copy
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_to_buffer_copy_full() {
    let src = vec![0xDEu8, 0xAD, 0xBE, 0xEF];
    let mut be = run(&[
        Cmd::CreateBuffer(1, buffer(4, ir_buffer_usage::COPY_SRC | ir_buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buffer(4, ir_buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: src.clone() },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 2, dst_offset: 0, size: 4 }],
            signal: None,
        }),
    ]);
    let mut out = [0u8; 4];
    be.read_buffer(BufferId(2), 0, &mut out).unwrap();
    assert_eq!(out, src.as_slice());
}

#[test]
fn buffer_to_buffer_copy_with_offsets() {
    // Copy a 2-byte window from the middle of src into the tail of dst; the rest of dst stays zero.
    let mut be = run(&[
        Cmd::CreateBuffer(1, buffer(6, ir_buffer_usage::COPY_SRC | ir_buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buffer(6, ir_buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![10, 11, 12, 13, 14, 15] },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 2, dst: 2, dst_offset: 4, size: 2 }],
            signal: None,
        }),
    ]);
    let mut out = [0u8; 6];
    be.read_buffer(BufferId(2), 0, &mut out).unwrap();
    assert_eq!(out, [0, 0, 0, 0, 12, 13]);
}

// -------------------------------------------------------------------------------------------------
// texture clear + readback (render-pass clear and ClearRect)
// -------------------------------------------------------------------------------------------------

/// A 1x1 texture cleared via a render pass reads back as the packed clear color.
#[test]
fn texture_clear_rgba8_readback_red() {
    let mut be = run(&[
        Cmd::CreateTexture(1, texture(1, 1, TextureFormat::Rgba8Unorm, ir_texture_usage::RENDER_TARGET | ir_texture_usage::COPY_SRC)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]);
    let mut px = [0u8; 4];
    be.read_texture(TextureId(1), &mut px).unwrap();
    assert_eq!(px, [255, 0, 0, 255]);
}

/// BGRA channel order must be honored: a "red" clear packs as B=0, G=0, R=255, A=255.
#[test]
fn texture_clear_bgra8_channel_order() {
    let mut be = run(&[
        Cmd::CreateTexture(1, texture(1, 1, TextureFormat::Bgra8Unorm, ir_texture_usage::RENDER_TARGET | ir_texture_usage::COPY_SRC)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]);
    let mut px = [0u8; 4];
    be.read_texture(TextureId(1), &mut px).unwrap();
    assert_eq!(px, [0, 0, 255, 255]); // B, G, R, A
}

/// A full-surface clear fills every texel of a 2x2 target with the same packed color.
#[test]
fn texture_clear_fills_all_texels() {
    let mut be = run(&[
        Cmd::CreateTexture(1, texture(2, 2, TextureFormat::Rgba8Unorm, ir_texture_usage::RENDER_TARGET | ir_texture_usage::COPY_SRC)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 1.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]);
    let mut px = [0u8; 16];
    be.read_texture(TextureId(1), &mut px).unwrap();
    let green = [0u8, 255, 0, 255];
    for texel in px.chunks_exact(4) {
        assert_eq!(texel, green);
    }
}

/// Clamped normalization: a mid-gray (0.5) clear rounds to 128 (round-half-up: 0.5*255+0.5 = 128.0).
#[test]
fn texture_clear_midgray_rounds_to_128() {
    let mut be = run(&[
        Cmd::CreateTexture(1, texture(1, 1, TextureFormat::Rgba8Unorm, ir_texture_usage::RENDER_TARGET)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.5, 0.5, 0.5, 0.5], store: true }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]);
    let mut px = [0u8; 4];
    be.read_texture(TextureId(1), &mut px).unwrap();
    assert_eq!(px, [128, 128, 128, 128]);
}

/// `ClearRect` scoped to a sub-rectangle touches only the covered texels; the rest stay zero.
#[test]
fn clear_rect_scopes_to_subrectangle() {
    let mut be = run(&[
        Cmd::CreateTexture(1, texture(2, 2, TextureFormat::Rgba8Unorm, ir_texture_usage::RENDER_TARGET | ir_texture_usage::COPY_SRC)),
        Cmd::Submit(CommandBuffer {
            // Clear only the top-left texel (0,0) 1x1 to red; leave the other three zero.
            encoder: vec![Enc::ClearRect { texture: 1, x: 0, y: 0, w: 1, h: 1, color: [1.0, 0.0, 0.0, 1.0] }],
            signal: None,
        }),
    ]);
    let mut px = [0u8; 16];
    be.read_texture(TextureId(1), &mut px).unwrap();
    // texel (0,0) red, remaining three zeroed
    assert_eq!(&px[0..4], &[255, 0, 0, 255]);
    assert_eq!(&px[4..16], &[0u8; 12]);
}

// -------------------------------------------------------------------------------------------------
// texture -> buffer readback copy (the IR readback path, distinct from the test-only read_texture)
// -------------------------------------------------------------------------------------------------

#[test]
fn texture_clear_then_copy_to_buffer() {
    let mut be = run(&[
        Cmd::CreateTexture(1, texture(1, 1, TextureFormat::Rgba8Unorm, ir_texture_usage::RENDER_TARGET | ir_texture_usage::COPY_SRC)),
        Cmd::CreateBuffer(1, buffer(4, ir_buffer_usage::COPY_DST)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 1.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::EndRenderPass,
                Enc::CopyTextureToBuffer { src: 1, mip: 0, width: 1, height: 1, dst: 1, dst_offset: 0, bytes_per_row: 4 },
            ],
            signal: None,
        }),
    ]);
    let mut out = [0u8; 4];
    be.read_buffer(BufferId(1), 0, &mut out).unwrap();
    assert_eq!(out, [0, 0, 255, 255]); // blue
}

// -------------------------------------------------------------------------------------------------
// compute dispatch — PTX kernel executed on the CPU oracle
// -------------------------------------------------------------------------------------------------

/// The software backend advertises compute; if a real backend does not, its conformance run should
/// skip the compute cases the same way. Guarded so the suite stays honest against any executor.
fn backend_supports_compute() -> bool {
    make_backend().capabilities().supports_compute
}

/// A minimal PTX kernel that stores the constant `1.0f` into its single global pointer argument.
/// Region 0 (binding 1) is the output; binding 0 is the parameter blob.
const STORE_ONE_PTX: &str = r#"
    .entry store_one(.param .u64 p) {
        ld.param.u64 %rd1, [p];
        cvta.to.global.u64 %rd2, %rd1;
        mov.f32 %f1, 0f3F800000;
        st.global.f32 [%rd2], %f1;
        ret;
    }
"#;

fn kernel_words(ptx: &str, entry: &str, block: [u32; 3]) -> Vec<u32> {
    let words = KernelDescriptor { ptx: ptx.to_string(), entry: entry.to_string(), block }.to_words();
    assert_eq!(words[0], KERNEL_MAGIC, "descriptor must carry the kernel magic");
    words
}

#[test]
fn compute_dispatch_writes_constant_into_buffer() {
    if !backend_supports_compute() {
        return;
    }
    let words = kernel_words(STORE_ONE_PTX, "store_one", [1, 1, 1]);
    let mut be = run(&[
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: words },
        Cmd::CreateComputePipeline(1, ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "store_one".into() }, label: String::new() }),
        // binding 0 = param blob (one u64 pointer = 8 bytes); binding 1 = output region 0 (one f32).
        Cmd::CreateBuffer(1, buffer(8, ir_buffer_usage::STORAGE)),
        Cmd::CreateBuffer(2, buffer(4, ir_buffer_usage::STORAGE | ir_buffer_usage::COPY_SRC)),
        Cmd::CreateBindGroup(1, BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 8 } },
                BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 4 } },
            ],
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ]);
    let mut out = [0u8; 4];
    be.read_buffer(BufferId(2), 0, &mut out).unwrap();
    let got = f32::from_le_bytes(out);
    assert_eq!(got, 1.0, "kernel must store 1.0f into region 0");
}

/// An elementwise vector add over N=4 lanes: c[i] = a[i] + b[i], executed through the full IR path.
/// Uses the crate's canonical reference kernel ([`VECADD_PTX`]): three pointer params (a,b,c → regions
/// 0,1,2 at bindings 1,2,3) and a scalar `n` at param-blob offset 24.
#[test]
fn compute_vecadd_elementwise() {
    if !backend_supports_compute() {
        return;
    }
    let words = kernel_words(VECADD_PTX, "vecadd", [4, 1, 1]);

    let n = 4u32;
    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let to_bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();

    // Param blob layout: three u64 pointers (values ignored by the interpreter) then n at offset 24.
    let mut param = vec![0u8; 28];
    param[24..28].copy_from_slice(&n.to_le_bytes());

    let mut be = run(&[
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: words },
        Cmd::CreateComputePipeline(1, ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "vecadd".into() }, label: String::new() }),
        Cmd::CreateBuffer(1, buffer(28, ir_buffer_usage::STORAGE)),         // params (binding 0)
        Cmd::CreateBuffer(2, buffer(16, ir_buffer_usage::STORAGE)),         // a -> region 0 (binding 1)
        Cmd::CreateBuffer(3, buffer(16, ir_buffer_usage::STORAGE)),         // b -> region 1 (binding 2)
        Cmd::CreateBuffer(4, buffer(16, ir_buffer_usage::STORAGE | ir_buffer_usage::COPY_SRC)), // c -> region 2 (binding 3)
        Cmd::WriteBuffer { id: 1, offset: 0, data: param },
        Cmd::WriteBuffer { id: 2, offset: 0, data: to_bytes(&a) },
        Cmd::WriteBuffer { id: 3, offset: 0, data: to_bytes(&b) },
        Cmd::CreateBindGroup(1, BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 28 } },
                BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 16 } },
                BindEntry { binding: 2, resource: BindResource::Buffer { id: 3, offset: 0, size: 16 } },
                BindEntry { binding: 3, resource: BindResource::Buffer { id: 4, offset: 0, size: 16 } },
            ],
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ]);
    let mut out = [0u8; 16];
    be.read_buffer(BufferId(4), 0, &mut out).unwrap();
    let got: Vec<f32> = out.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0]);
}

// -------------------------------------------------------------------------------------------------
// re-exports of the IR usage-flag modules under unambiguous names for readability above
// -------------------------------------------------------------------------------------------------

use hl_gpu::ir::buffer_usage as ir_buffer_usage;
use hl_gpu::ir::texture_usage as ir_texture_usage;
