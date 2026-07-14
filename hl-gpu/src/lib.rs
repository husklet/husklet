//! hl-gpu — the host-GPU-agnostic command IR, wire protocol, resource-handle table, and `GpuBackend`
//! executor abstraction for hl's rung-3 GPU forwarding and its CUDA-on-Metal device simulation.
//!
//! See `docs/ideas/RENDERING_GPU_BACKENDS.md` (the forward-level/IR decision + backend abstraction +
//! optimization plan) and `docs/ideas/CUDA_ON_METAL.md` (the "show the Linux container a CUDA device,
//! run it on the host Metal GPU" design).
//!
//! ## What is real here vs designed-only
//! This crate is **pure `std`, no serde, hand-rolled wire** — the `hl-term-core` discipline — so it
//! `cargo test`s HEADLESS on the Linux dev box with no GPU/display/CUDA and no crates.io. The pieces
//! that need a real GPU (`hl-gpu-wgpu`'s real-Metal `WgpuBackend`, a future `CudaBackend`) live in
//! separate crates; here we
//! prototype and test the IR, the wire round-trip, the resource table, a recording mock backend, a real
//! CPU software backend (clear/copy/readback), the command ring, and the CUDA→IR translation shim.
//!
//! ```
//! use hl_gpu::ir::*;
//! use hl_gpu::{software::SoftwareBackend, replay};
//! // Clear a texture to red and read the pixel back — entirely on the CPU, no GPU needed.
//! let mut be = SoftwareBackend::new();
//! let cmds = vec![
//!     Cmd::CreateTexture(1, TextureDesc { width: 1, height: 1, depth: 1, mip_levels: 1,
//!         sample_count: 1, dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
//!         usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC, label: String::new() }),
//!     Cmd::Submit(CommandBuffer {
//!         encoder: vec![
//!             Enc::BeginRenderPass { color: vec![ColorAttachment {
//!                 texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
//!                 depth: None },
//!             Enc::EndRenderPass ],
//!         signal: None }),
//! ];
//! replay::replay(&mut be, &cmds).unwrap();
//! let mut px = [0u8; 4];
//! use hl_gpu::backend::GpuBackend;
//! be.read_texture(hl_gpu::id::TextureId(1), &mut px).unwrap();
//! assert_eq!(px, [255, 0, 0, 255]);
//! ```

pub mod backend;
pub mod cuda;
pub mod id;
pub mod ir;
pub mod limits;
pub mod mock;
pub mod ptx;
pub mod replay;
pub mod ring;
pub mod software;
pub mod wire;

// The runtime-integration seam: hl-gpu's implementor of hl-jit's device-provider interface, so the
// container launcher wires a GPU generically instead of hardcoding CUDA/IOSurface/Wayland specifics.
// Gated behind the `runtime` feature (pulls in `hl-jit`); the headless IR/wire core above stays pure-std.
#[cfg(feature = "runtime")]
pub mod integration;

pub use backend::{Capabilities, GpuBackend, PresentKind, PresentToken};
pub use id::{
    BindGroupId, BufferId, FenceId, PipelineId, ResourceTable, SamplerId, ShaderId, SurfaceId,
    TextureId,
};

/// The crate's single error type. Every fallible boundary — wire decode, handle-table lifecycle,
/// backend op — returns this, so a malformed guest stream is a typed error, never UB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// Ran off the end of the input while decoding.
    ShortBuffer,
    /// Unknown top-level command / encoder tag byte.
    BadTag(u32),
    /// An enum field held a value outside its defined set.
    BadEnum { what: &'static str, val: u32 },
    /// A string field was not valid UTF-8.
    Utf8,
    /// A length-prefixed frame held extra bytes after its message decoded (trailing garbage).
    TrailingBytes,
    /// A boolean wire field held a byte other than the canonical `0`/`1`.
    NonCanonicalBool(u8),
    /// Created an id that is already live.
    DuplicateId { kind: &'static str, id: u32 },
    /// Referenced an id that was never created or was already freed.
    UnknownId { kind: &'static str, id: u32 },
    /// The backend does not implement this operation.
    Unsupported(&'static str),
    /// A copy/write/read fell outside the resource's bounds.
    OutOfBounds,
    /// A render-state float (viewport/scissor coordinate, clear color, depth) decoded as non-finite
    /// (NaN or ±∞) at the wire boundary — a malformed/hostile payload rejected before it can poison a
    /// backend's viewport/clear/transform. The `&'static str` names the offending field.
    NonFinite(&'static str),
    /// A descriptor/command field was structurally invalid (zero size, contradictory range,
    /// unsupported dimensionality, missing required usage bit, etc.) — a validation rejection
    /// distinct from a plain bounds violation.
    Invalid(&'static str),
    /// A negotiated per-object or aggregate executor resource limit was exceeded.
    ResourceLimit(&'static str),
    /// Malformed or unsupported PTX while compiling a kernel to hl-GPU kernel IR.
    Ptx(String),
    /// Higher-level decode context wrapped around a low-level wire error.
    Decode(String),
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuError::ShortBuffer => write!(f, "short buffer while decoding"),
            GpuError::BadTag(t) => write!(f, "bad command/encoder tag {t}"),
            GpuError::BadEnum { what, val } => write!(f, "bad {what} enum value {val}"),
            GpuError::Utf8 => write!(f, "invalid utf-8 in string field"),
            GpuError::TrailingBytes => write!(f, "trailing bytes after framed message"),
            GpuError::NonCanonicalBool(b) => write!(f, "non-canonical boolean wire byte {b}"),
            GpuError::DuplicateId { kind, id } => write!(f, "duplicate {kind} id {id}"),
            GpuError::UnknownId { kind, id } => write!(f, "unknown/freed {kind} id {id}"),
            GpuError::Unsupported(op) => write!(f, "backend does not support {op}"),
            GpuError::OutOfBounds => write!(f, "access out of bounds"),
            GpuError::NonFinite(field) => write!(f, "non-finite float in render state: {field}"),
            GpuError::Invalid(m) => write!(f, "invalid argument: {m}"),
            GpuError::ResourceLimit(m) => write!(f, "executor resource limit: {m}"),
            GpuError::Ptx(m) => write!(f, "ptx: {m}"),
            GpuError::Decode(m) => write!(f, "decode: {m}"),
        }
    }
}

impl std::error::Error for GpuError {}

pub type Result<T> = std::result::Result<T, GpuError>;

// ===================================================================================================
// tests — all headless, no GPU/display/CUDA/network
// ===================================================================================================
#[cfg(test)]
mod tests {
    use crate::backend::GpuBackend;
    use crate::cuda::*;
    use crate::id::*;
    use crate::ir::*;
    use crate::mock::{expect_err, Rec, RecordingBackend};
    use crate::ring::CommandRing;
    use crate::software::SoftwareBackend;
    use crate::{replay, GpuError};

    fn sample_stream() -> Vec<Cmd> {
        vec![
            Cmd::CreateBuffer(
                1,
                BufferDesc { size: 256, usage: buffer_usage::VERTEX | buffer_usage::COPY_DST, label: "vb".into() },
            ),
            Cmd::WriteBuffer { id: 1, offset: 0, data: vec![1, 2, 3, 4, 5, 6, 7, 8] },
            Cmd::CreateTexture(
                2,
                TextureDesc {
                    width: 64, height: 32, depth: 1, mip_levels: 1, sample_count: 1,
                    dim: TextureDim::D2, format: TextureFormat::Bgra8Unorm,
                    usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT, label: "rt".into(),
                },
            ),
            Cmd::CreateShader { id: 3, kind: ShaderPayloadKind::SpirV, spirv: vec![0x0723_0203, 0x0001_0000, 42, 7] },
            Cmd::CreateShader { id: 4, kind: ShaderPayloadKind::SpirV, spirv: vec![0x0723_0203, 0x0001_0000, 99] },
            Cmd::CreateRenderPipeline(
                5,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 3, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 4, entry: "fs_main".into() }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 16, step_mode: 0,
                        attrs: vec![VertexAttr { location: 0, format: 23, offset: 0 }],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Bgra8Unorm,
                        blend: Some(BlendState { src_color: 1, dst_color: 0, op_color: 0, src_alpha: 1, dst_alpha: 0, op_alpha: 0 }),
                        write_mask: 0xF,
                    }],
                    depth: None, topology: Topology::TriangleList, cull: 2, front_face: 0, label: "pipe".into(),
                },
            ),
            Cmd::CreateBindGroup(
                6,
                BindGroupDesc { set: 0, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 256 } }] },
            ),
            Cmd::CreateSurface(7, SurfaceDesc { width: 64, height: 32, format: TextureFormat::Bgra8Unorm, hlp_surface: 100 }),
            Cmd::CreateFence(8),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 2, load: LoadOp::Clear, clear: [0.1, 0.2, 0.3, 1.0], store: true }], depth: None },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: 64.0, h: 32.0, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetPipeline(5),
                    Enc::SetBindGroup { index: 0, group: 6 },
                    Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: Some((8, 1)),
            }),
            Cmd::WaitFence { id: 8, value: 1 },
            Cmd::Present { surface: 7, texture: 2 },
        ]
    }

    #[test]
    fn wire_round_trip_every_command() {
        let cmds = sample_stream();
        let bytes = encode_stream(&cmds);
        let back = decode_stream(&bytes).expect("decode");
        assert_eq!(cmds, back, "stream must survive encode→decode unchanged");
    }

    #[test]
    fn wire_round_trip_per_command_frames() {
        // Each command self-delimited as a ring frame decodes back to itself.
        for c in sample_stream() {
            let framed = c.frame();
            let mut d = crate::wire::Decoder::new(&framed);
            let body = d.bytes().unwrap();
            let mut bd = crate::wire::Decoder::new(body);
            assert_eq!(Cmd::decode(&mut bd).unwrap(), c);
        }
    }

    #[test]
    fn framed_command_decode_rejects_trailing_bytes() {
        use crate::wire::{Decoder, Encoder};
        let cmd = Cmd::CreateFence(1);
        // A well-formed frame round-trips.
        let good = cmd.frame();
        let mut d = Decoder::new(&good);
        assert_eq!(Cmd::decode_frame(&mut d).unwrap(), cmd);
        // Now append a trailing byte inside the frame body: the frame is malformed.
        let mut e = Encoder::new();
        e.frame(|inner| {
            cmd.encode(inner);
            inner.u8(0xEE); // trailing garbage after a valid command
        });
        let framed = e.into_vec();
        let mut d = Decoder::new(&framed);
        assert_eq!(Cmd::decode_frame(&mut d), Err(GpuError::TrailingBytes));
    }

    #[test]
    fn decoder_does_not_preallocate_on_bogus_counts() {
        use crate::wire::Encoder;
        // A CreateBindGroup claiming ~4 billion entries but with an empty body must fail cleanly as a
        // decode/short-buffer error — never attempt a multi-gigabyte Vec::with_capacity first.
        let mut e = Encoder::new();
        e.u8(crate::ir::tag::CREATE_BIND_GROUP);
        e.u32(1); // id
        e.u32(0); // set
        e.u32(0xFFFF_FFFF); // entry count = ~4 billion, no entries follow
        let bytes = e.into_vec();
        let err = decode_stream(&bytes).unwrap_err();
        assert!(matches!(&err, GpuError::Decode(m) if m.contains("short buffer")));
    }

    #[test]
    fn decode_rejects_truncation_and_bad_tags() {
        let bytes = encode_stream(&sample_stream());
        // truncate mid-stream -> contextual ShortBuffer, never a panic
        let err = decode_stream(&bytes[..bytes.len() - 3]).unwrap_err();
        assert!(matches!(&err, GpuError::Decode(m) if m.contains("command") && m.contains("short buffer")));
        // a bogus leading tag byte
        let err = decode_stream(&[250, 0, 0, 0, 0]).unwrap_err();
        assert!(matches!(&err, GpuError::Decode(m) if m.contains("command 0") && m.contains("bad command/encoder tag 250")));
    }

    #[test]
    fn handle_table_lifecycle() {
        let mut t: ResourceTable<u32> = ResourceTable::new("buffer");
        assert!(t.insert(1, 111).is_ok());
        assert!(t.insert(2, 222).is_ok());
        // duplicate create
        assert_eq!(t.insert(1, 999), Err(GpuError::DuplicateId { kind: "buffer", id: 1 }));
        assert_eq!(*t.get(1).unwrap(), 111);
        assert_eq!(t.len(), 2);
        // free then use-after-free
        assert_eq!(t.remove(1).unwrap(), 111);
        assert_eq!(t.get(1), Err(GpuError::UnknownId { kind: "buffer", id: 1 }));
        // double free
        assert_eq!(t.remove(1), Err(GpuError::UnknownId { kind: "buffer", id: 1 }));
        // reuse assigns a fresh, distinct generation (from a global monotonic counter), so a stale
        // reference to the old allocation can be told apart from the reused one; id 2 is untouched.
        let g_before = t.generation(2);
        assert!(t.insert(1, 333).is_ok());
        assert_ne!(t.generation(1), g_before); // id 1 reused → new, distinct generation
        assert!(t.generation(1).is_some());
        assert_eq!(t.generation(2), g_before);
    }

    #[test]
    fn mock_backend_records_replayed_sequence() {
        let mut be = RecordingBackend::new();
        let presents = replay::replay(&mut be, &sample_stream()).unwrap();
        assert_eq!(presents.len(), 1);
        assert_eq!(presents[0].surface, 7);
        // spot-check the recorded high-level sequence reached the host in order
        use Rec::*;
        let expect = vec![
            CreateBuffer(1, 256, buffer_usage::VERTEX | buffer_usage::COPY_DST),
            WriteBuffer(1, 0, 8),
            CreateTexture(2, 64, 32),
            CreateShader(3, 4),
            CreateShader(4, 3),
            CreateRenderPipeline(5),
            CreateBindGroup(6),
            CreateSurface(7),
            CreateFence(8),
            Submit(7),
            BeginRenderPass(1),
            SetPipeline(5),
            Draw(3, 1),
            EndRenderPass,
            WaitFence(8, 1),
            Present(7, 2),
        ];
        assert_eq!(be.log, expect);
    }

    #[test]
    fn mock_backend_enforces_lifecycle_through_the_trait() {
        let mut be = RecordingBackend::new();
        // pipeline referencing a non-existent shader must fail at the trait boundary
        let bad = Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef { module: 99, entry: "x".into() },
                fragment: None, vertex_buffers: vec![], color_targets: vec![],
                depth: None, topology: Topology::TriangleList, cull: 0, front_face: 0, label: String::new(),
            },
        );
        assert_eq!(
            expect_err(replay::apply(&mut be, &bad)),
            GpuError::UnknownId { kind: "shader", id: 99 }
        );
    }

    #[test]
    fn wire_then_replay_into_mock_is_equivalent() {
        // Guest encodes → bytes cross the ring → host decodes+replays: same result as in-memory replay.
        let bytes = encode_stream(&sample_stream());
        let mut be = RecordingBackend::new();
        let presents = replay::replay_stream(&mut be, &bytes).unwrap();
        assert_eq!(presents.len(), 1);
        assert_eq!(be.log.first(), Some(&Rec::CreateBuffer(1, 256, buffer_usage::VERTEX | buffer_usage::COPY_DST)));
    }

    #[test]
    fn software_backend_clear_and_readback() {
        let mut be = SoftwareBackend::new();
        let cmds = vec![
            Cmd::CreateTexture(
                1,
                TextureDesc { width: 2, height: 2, depth: 1, mip_levels: 1, sample_count: 1,
                    dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC, label: String::new() },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 1.0, 0.0, 1.0], store: true }], depth: None },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        replay::replay(&mut be, &cmds).unwrap();
        let mut px = vec![0u8; 2 * 2 * 4];
        be.read_texture(TextureId(1), &mut px).unwrap();
        // every texel is green
        for chunk in px.chunks(4) {
            assert_eq!(chunk, [0, 255, 0, 255]);
        }
    }

    #[test]
    fn software_backend_bgra_channel_order() {
        let mut be = SoftwareBackend::new();
        be.create_texture(
            TextureId(1),
            &TextureDesc { width: 1, height: 1, depth: 1, mip_levels: 1, sample_count: 1,
                dim: TextureDim::D2, format: TextureFormat::Bgra8Unorm,
                usage: texture_usage::RENDER_TARGET, label: String::new() },
        ).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }], depth: None },
                Enc::EndRenderPass,
            ],
            signal: None,
        }).unwrap();
        let mut px = [0u8; 4];
        be.read_texture(TextureId(1), &mut px).unwrap();
        assert_eq!(px, [0, 0, 255, 255], "red in BGRA is 00 00 FF FF");
    }

    #[test]
    fn software_backend_buffer_write_copy_readback() {
        let mut be = SoftwareBackend::new();
        be.create_buffer(BufferId(1), &BufferDesc { size: 16, usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST, label: String::new() }).unwrap();
        be.create_buffer(BufferId(2), &BufferDesc { size: 16, usage: buffer_usage::COPY_DST, label: String::new() }).unwrap();
        be.write_buffer(BufferId(1), 4, &[9, 8, 7, 6]).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 4, dst: 2, dst_offset: 0, size: 4 }],
            signal: None,
        }).unwrap();
        let mut out = [0u8; 4];
        be.read_buffer(BufferId(2), 0, &mut out).unwrap();
        assert_eq!(out, [9, 8, 7, 6]);
        // out-of-bounds write is a typed error
        assert_eq!(be.write_buffer(BufferId(1), 14, &[1, 2, 3, 4]), Err(GpuError::OutOfBounds));
    }

    #[test]
    fn software_backend_copy_buffer_to_texture_honors_bytes_per_row() {
        // A 2x2 RGBA8 texture: 8 tightly-packed bytes per row. The *source* buffer pads each row up to
        // bytes_per_row=12 (4 padding bytes of 0xaa). Software replay must copy only the real 8 bytes of
        // each row, never letting padding bytes become visible texels.
        let mut be = SoftwareBackend::new();
        be.create_texture(
            TextureId(1),
            &TextureDesc { width: 2, height: 2, depth: 1, mip_levels: 1, sample_count: 1,
                dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::COPY_DST, label: String::new() },
        ).unwrap();
        be.create_buffer(
            BufferId(1),
            &BufferDesc { size: 24, usage: buffer_usage::COPY_SRC, label: String::new() },
        ).unwrap();
        // row0 pixels, 4 bytes padding, row1 pixels, 4 bytes padding.
        let mut src = Vec::new();
        src.extend_from_slice(&[0, 1, 2, 3, 4, 5, 6, 7]); // row 0 (2 texels)
        src.extend_from_slice(&[0xaa; 4]); // padding
        src.extend_from_slice(&[16, 17, 18, 19, 20, 21, 22, 23]); // row 1
        src.extend_from_slice(&[0xaa; 4]); // padding
        be.write_buffer(BufferId(1), 0, &src).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 1, src_offset: 0, bytes_per_row: 12, dst: 1, mip: 0, width: 2, height: 2,
            }],
            signal: None,
        }).unwrap();
        let mut px = [0u8; 16];
        be.read_texture(TextureId(1), &mut px).unwrap();
        assert_eq!(
            px,
            [0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23],
            "padding bytes (0xaa) leaked into texels — bytes_per_row was ignored"
        );
    }

    #[test]
    fn software_backend_present_format_check() {
        let mut be = SoftwareBackend::new();
        be.create_surface(SurfaceId(1), &SurfaceDesc { width: 4, height: 4, format: TextureFormat::Bgra8Unorm, hlp_surface: 1 }).unwrap();
        be.create_texture(TextureId(1), &TextureDesc { width: 4, height: 4, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2, format: TextureFormat::Bgra8Unorm, usage: texture_usage::PRESENT, label: String::new() }).unwrap();
        let tok = be.present(SurfaceId(1), TextureId(1)).unwrap();
        assert!(tok.format_ok);
        assert_eq!((tok.width, tok.height), (4, 4));
    }

    #[test]
    fn command_ring_framing_and_backpressure() {
        let mut ring = CommandRing::with_capacity(64);
        let cmds = vec![
            Cmd::CreateFence(1),
            Cmd::WaitFence { id: 1, value: 7 },
            Cmd::DestroyFence(1),
        ];
        // push framed commands
        let mut pushed = 0;
        for c in &cmds {
            let mut e = crate::wire::Encoder::new();
            c.encode(&mut e);
            if ring.push_frame(&e.buf) {
                pushed += 1;
            }
        }
        assert_eq!(pushed, cmds.len());
        // drain and decode
        let mut drained = Vec::new();
        while let Some(body) = ring.pop_frame() {
            let mut d = crate::wire::Decoder::new(&body);
            drained.push(Cmd::decode(&mut d).unwrap());
        }
        assert_eq!(drained, cmds);

        // backpressure: a frame larger than free space is refused, ring unchanged
        let mut tiny = CommandRing::with_capacity(8);
        assert!(!tiny.push_frame(&[0u8; 100]));
        assert!(tiny.is_empty());
    }

    // -------- CUDA-on-Metal simulation --------

    #[test]
    fn cuda_device_presence_strings() {
        let d = CudaDeviceDesc::apple_default(8 << 30); // 8 GiB
        assert_eq!(d.compute_capability_str(), "8.6");
        assert_eq!(d.total_mem, 8 << 30);
        let smi = d.nvidia_smi_l_line(0);
        assert!(smi.starts_with("GPU 0: hl Metal (CUDA-sim) Device (UUID: GPU-"));
    }

    #[test]
    fn ptx_entry_point_parsing() {
        let ptx = r#"
            .version 7.0
            .target sm_86
            .visible .entry vecadd(
                .param .u64 a, .param .u64 b, .param .u64 c) { ret; }
            .entry scale (.param .u64 x) { ret; }
        "#;
        let m = PtxModule::parse(ptx);
        assert_eq!(m.entries, vec!["vecadd".to_string(), "scale".to_string()]);
    }

    #[test]
    fn cuda_memory_path_runs_on_software_backend() {
        // The device-presence + memory path executes end-to-end with NO GPU: alloc, H2D, D2H.
        let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(4 << 30));
        let mut be = SoftwareBackend::new();

        let (dptr, create) = ctx.mem_alloc(32);
        replay::apply(&mut be, &create).unwrap();

        let host_in: Vec<u8> = (0..32u8).collect();
        let h2d = ctx.memcpy_htod(dptr, &host_in).unwrap();
        replay::apply(&mut be, &h2d).unwrap();

        // cuMemcpyDtoH: resolve ptr → (buffer, offset) then read back via the backend
        let (buf, off) = ctx.resolve(dptr).unwrap();
        let mut host_out = vec![0u8; 32];
        be.read_buffer(buf, off, &mut host_out).unwrap();
        assert_eq!(host_in, host_out, "H2D then D2H round-trips through the simulated device");

        // offset pointer arithmetic resolves within the allocation
        let (buf2, off2) = ctx.resolve(DevicePtr(dptr.0 + 8)).unwrap();
        assert_eq!(buf2, buf);
        assert_eq!(off2, 8);

        // free
        let free = ctx.mem_free(dptr).unwrap();
        replay::apply(&mut be, &free).unwrap();
        assert!(ctx.resolve(dptr).is_none());
    }

    #[test]
    fn cuda_launch_emits_compute_dispatch() {
        let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(4 << 30));
        let mut be = SoftwareBackend::new();

        // two device buffers as kernel args
        let (a, ca) = ctx.mem_alloc(256);
        let (b, cb) = ctx.mem_alloc(256);
        replay::apply(&mut be, &ca).unwrap();
        replay::apply(&mut be, &cb).unwrap();

        let module = ctx.module_load(".visible .entry saxpy(.param .u64 x){ ret; }");
        let func = ctx.module_get_function(module, "saxpy").expect("entry");

        let ir = ctx.launch(func, (64, 1, 1), (256, 1, 1), &[KernelArg::Ptr(a), KernelArg::Ptr(b)]);
        // A launch's transient parameter buffer + bind group must be released after the (synchronous)
        // submit, so repeated launches don't leak backend resources.
        let param_creates = ir.iter().filter(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.label == "kernel-params")).count();
        let bg_creates = ir.iter().filter(|c| matches!(c, Cmd::CreateBindGroup(..))).count();
        let buf_destroys = ir.iter().filter(|c| matches!(c, Cmd::DestroyBuffer(..))).count();
        let bg_destroys = ir.iter().filter(|c| matches!(c, Cmd::DestroyBindGroup(..))).count();
        assert_eq!(param_creates, buf_destroys, "each launch's param buffer is destroyed");
        assert_eq!(bg_creates, bg_destroys, "each launch's bind group is destroyed");
        assert!(bg_destroys >= 1);
        // first launch creates shader+pipeline+bindgroup then submits
        for c in &ir {
            replay::apply(&mut be, c).unwrap();
        }
        assert_eq!(be.dispatches, 1, "the launch reached the executor as one compute dispatch");

        // a second launch of the same function reuses the pipeline (no new shader/pipeline create)
        let ir2 = ctx.launch(func, (32, 1, 1), (256, 1, 1), &[KernelArg::Ptr(a), KernelArg::Ptr(b)]);
        let creates = ir2.iter().filter(|c| matches!(c, Cmd::CreateComputePipeline(..) | Cmd::CreateShader { .. })).count();
        assert_eq!(creates, 0, "pipeline cache hit on repeat launch");
        for c in &ir2 {
            replay::apply(&mut be, c).unwrap();
        }
        assert_eq!(be.dispatches, 2);
    }

    /// THE MILESTONE: a real PTX kernel (vector-add) allocates device memory, uploads inputs, launches
    /// through the CUDA→hl-GPU-IR translation, executes on the software backend, and reads back numerically
    /// correct results — the whole `libcuda → PTX → hl-GPU IR → backend` chain, headless, no GPU.
    #[test]
    fn cuda_vecadd_executes_end_to_end_on_software_backend() {
        let n = 1024usize;
        let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(4 << 30));
        let mut be = SoftwareBackend::new();

        // cuMemAlloc a, b, c
        let (a, ca) = ctx.mem_alloc((n * 4) as u64);
        let (b, cb) = ctx.mem_alloc((n * 4) as u64);
        let (c, cc) = ctx.mem_alloc((n * 4) as u64);
        for cmd in [&ca, &cb, &cc] {
            replay::apply(&mut be, cmd).unwrap();
        }

        // host inputs → cuMemcpyHtoD
        let ha: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let hb: Vec<f32> = (0..n).map(|i| (n - i) as f32 * 0.5).collect();
        let bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        replay::apply(&mut be, &ctx.memcpy_htod(a, &bytes(&ha)).unwrap()).unwrap();
        replay::apply(&mut be, &ctx.memcpy_htod(b, &bytes(&hb)).unwrap()).unwrap();

        // cuModuleLoadData(vecadd PTX) + cuModuleGetFunction
        let m = ctx.module_load(crate::ptx::VECADD_PTX);
        let f = ctx.module_get_function(m, "vecadd").expect("entry vecadd");

        // cuLaunchKernel<<<ceil(n/256), 256>>>(a, b, c, n)
        let block = (256u32, 1, 1);
        let grid = ((n as u32).div_ceil(256), 1, 1);
        let args = [
            KernelArg::Ptr(a),
            KernelArg::Ptr(b),
            KernelArg::Ptr(c),
            KernelArg::Scalar((n as u32).to_le_bytes().to_vec()),
        ];
        for cmd in &ctx.launch(f, grid, block, &args) {
            replay::apply(&mut be, cmd).unwrap();
        }
        assert_eq!(be.dispatches, 1, "one compute dispatch reached the executor");

        // cuMemcpyDtoH(c) → assert c[i] == a[i] + b[i]
        let (cbuf, coff) = ctx.resolve(c).unwrap();
        let mut out = vec![0u8; n * 4];
        be.read_buffer(cbuf, coff, &mut out).unwrap();
        for i in 0..n {
            let got = f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(got, ha[i] + hb[i], "vecadd result mismatch at c[{i}]");
        }
    }

    /// The same vecadd, but the ENTIRE command stream (including the kernel descriptor words) is
    /// serialized to the wire and decoded+replayed on the other side — proving the kernel path survives
    /// the ring exactly like every other hl-GPU command.
    #[test]
    fn cuda_vecadd_survives_the_wire() {
        let n = 256usize;
        let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(4 << 30));

        let (a, ca) = ctx.mem_alloc((n * 4) as u64);
        let (b, cb) = ctx.mem_alloc((n * 4) as u64);
        let (c, cc) = ctx.mem_alloc((n * 4) as u64);
        let ha: Vec<f32> = (0..n).map(|i| (i as f32) * 3.0).collect();
        let hb: Vec<f32> = (0..n).map(|i| (i as f32) + 1.0).collect();
        let bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();

        let mut stream: Vec<Cmd> = vec![ca, cb, cc];
        stream.push(ctx.memcpy_htod(a, &bytes(&ha)).unwrap());
        stream.push(ctx.memcpy_htod(b, &bytes(&hb)).unwrap());
        let m = ctx.module_load(crate::ptx::VECADD_PTX);
        let f = ctx.module_get_function(m, "vecadd").unwrap();
        let args = [
            KernelArg::Ptr(a),
            KernelArg::Ptr(b),
            KernelArg::Ptr(c),
            KernelArg::Scalar((n as u32).to_le_bytes().to_vec()),
        ];
        stream.extend(ctx.launch(f, ((n as u32).div_ceil(128), 1, 1), (128, 1, 1), &args));

        // guest encodes → bytes cross the ring → host decodes + replays.
        let wire = encode_stream(&stream);
        let mut be = SoftwareBackend::new();
        replay::replay_stream(&mut be, &wire).unwrap();

        let (cbuf, coff) = ctx.resolve(c).unwrap();
        let mut out = vec![0u8; n * 4];
        be.read_buffer(cbuf, coff, &mut out).unwrap();
        for i in 0..n {
            let got = f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(got, ha[i] + hb[i], "wire vecadd mismatch at c[{i}]");
        }
    }

    #[test]
    fn capabilities_advertise_present_kinds() {
        let be = SoftwareBackend::new();
        let caps = be.capabilities();
        assert!(caps.unified_memory);
        assert!(caps.present_kinds.contains(&crate::PresentKind::Shm));
    }
}
