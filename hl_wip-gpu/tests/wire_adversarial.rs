//! EXTREME adversarial coverage for the protocol wire codec — the highest-value target, since a real
//! remote peer sends UNTRUSTED bytes. Three families of guarantees are locked in here:
//!
//! 1. **Total decode robustness**: for EVERY encoder-op (`etag`) and EVERY top-level command (`tag`),
//!    truncating the encoded bytes at every prefix, corrupting the tag, and mutating fields must return a
//!    clean typed `Err` — NEVER a panic / OOB read / UB.
//! 2. **Byte-stability**: for ANY byte string the decoder ACCEPTS, `encode(decode(bytes)) == bytes`. The
//!    decoder normalizes nothing silently; a decodable frame re-encodes to itself, byte-for-byte. A single
//!    counterexample is a real bug (a producer/consumer desync).
//! 3. **Typed rejection of every malformed shape**: unknown tag/etag → `BadTag`; out-of-range enum →
//!    `BadEnum`; non-canonical bool → `NonCanonicalBool`; non-finite render float → `NonFinite`; trailing
//!    frame bytes → `TrailingBytes`; a bogus length prefix → `ShortBuffer` (no giant prealloc).
//!
//! Complements `tests/fuzz.rs` (random/bitflip/truncation on 2 streams) and `tests/roundtrip.rs`.

use std::panic::catch_unwind;

use hl_gpu::protocol::codec::wire::{Decoder, Encoder};
use hl_gpu::protocol::model::command::{etag, tag, Cmd, CommandBuffer, Enc};
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::protocol::model::kernel::{
    glsl_stage, GlslDescriptor, KernelDescriptor, GLSL_MAGIC, KERNEL_MAGIC, SPIRV_MAGIC,
};
use hl_gpu::{decode_stream, encode_stream, GpuError, ShaderPayloadKind, WIRE_VERSION};

// ---------------------------------------------------------------------------------------------------
// exhaustive op/command inventories (one canonical value of EVERY etag and EVERY tag)
// ---------------------------------------------------------------------------------------------------

/// One canonical, well-formed value of EVERY encoder op (all 22 etags), with finite floats + canonical
/// bools so each participates in the value-round-trip guarantee.
fn every_encoder_op() -> Vec<Enc> {
    let sub = TextureSubresource::base();
    let org = Origin3d::default();
    let ext = Extent3d { width: 4, height: 4, depth: 1 };
    vec![
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: 2,
                load: LoadOp::Clear,
                clear: [0.0, 0.5, 1.0, 1.0],
                store: true,
            }],
            depth: Some(DepthAttachment { texture: 3, load: LoadOp::Load, clear_depth: 1.0, clear_stencil: 7 }),
        },
        Enc::SetPipeline(5),
        Enc::SetBindGroup { index: 0, group: 6 },
        Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
        Enc::SetIndexBuffer { buffer: 1, offset: 0, format: IndexFormat::U16 },
        Enc::SetViewport { x: 0.0, y: 0.0, w: 4.0, h: 4.0, min_depth: 0.0, max_depth: 1.0 },
        Enc::SetScissor { x: 0, y: 0, w: 4, h: 4 },
        Enc::ClearRect { texture: 2, x: 0, y: 0, w: 2, h: 2, color: [1.0, 0.0, 0.0, 1.0] },
        Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
        Enc::DrawIndexed {
            index_count: 3,
            instance_count: 1,
            first_index: 0,
            base_vertex: -1,
            first_instance: 0,
        },
        Enc::EndRenderPass,
        Enc::BeginComputePass,
        Enc::Dispatch { x: 8, y: 1, z: 1 },
        Enc::EndComputePass,
        Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 1, dst_offset: 4, size: 4 },
        Enc::CopyBufferToTexture {
            src: 1,
            src_offset: 0,
            bytes_per_row: 16,
            dst: 2,
            mip: 0,
            width: 4,
            height: 4,
        },
        Enc::CopyTextureToBuffer {
            src: 2,
            mip: 0,
            width: 4,
            height: 4,
            dst: 1,
            dst_offset: 0,
            bytes_per_row: 16,
        },
        Enc::CopyTextureToTexture {
            src: 2,
            src_sub: sub,
            src_origin: org,
            dst: 9,
            dst_sub: sub,
            dst_origin: org,
            extent: ext,
        },
        Enc::BlitTexture {
            src: 2,
            src_sub: sub,
            src_origin: org,
            src_extent: ext,
            dst: 9,
            dst_sub: sub,
            dst_origin: org,
            dst_extent: ext,
            filter: Filter::Linear,
        },
        Enc::ResolveTexture {
            src: 2,
            src_sub: sub,
            src_origin: org,
            dst: 9,
            dst_sub: sub,
            dst_origin: org,
            extent: ext,
        },
        Enc::FillBuffer { buffer: 1, offset: 0, size: 8, value: 0xDEAD_BEEF },
        Enc::SetStencilReference { reference: 0x0000_00A5 },
    ]
}

/// One canonical value of EVERY top-level command (all 21 tags). Shader payloads lead with the magic that
/// matches their declared kind so each survives the value round-trip (the kind is re-derived on decode).
fn every_command() -> Vec<Cmd> {
    let kd = KernelDescriptor { ptx: "ret;".into(), entry: "k".into(), block: [64, 1, 1] };
    let gd = GlslDescriptor {
        stage: glsl_stage::FRAGMENT,
        entry: "fmain".into(),
        source: "#version 460\nvoid main(){}\n".into(),
    };
    vec![
        Cmd::CreateBuffer(1, BufferDesc { size: 256, usage: 0x3F, label: "vb".into() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![1, 2, 3, 4, 5, 6, 7, 8] },
        Cmd::CreateTexture(
            2,
            TextureDesc {
                width: 4,
                height: 4,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Bgra8Unorm,
                usage: 0x3F,
                label: "rt".into(),
            },
        ),
        Cmd::CreateSampler(
            3,
            SamplerDesc {
                min_filter: Filter::Linear,
                mag_filter: Filter::Nearest,
                mip_filter: Filter::Linear,
                address_u: AddressMode::Repeat,
                address_v: AddressMode::ClampToEdge,
                address_w: AddressMode::MirrorRepeat,
            },
        ),
        Cmd::CreateShader { id: 4, kind: ShaderPayloadKind::SpirV, spirv: vec![SPIRV_MAGIC, 1, 2, 3] },
        Cmd::CreateShader { id: 5, kind: ShaderPayloadKind::PtxKernel, spirv: kd.to_words() },
        Cmd::CreateShader { id: 6, kind: ShaderPayloadKind::Glsl, spirv: gd.to_words() },
        // A payload with no magic classifies as LegacyMsl.
        Cmd::CreateShader { id: 7, kind: ShaderPayloadKind::LegacyMsl, spirv: vec![0x4141_4141] },
        Cmd::CreateRenderPipeline(
            8,
            RenderPipelineDesc {
                vertex: ShaderRef { module: 4, entry: "vs".into() },
                fragment: Some(ShaderRef { module: 6, entry: "fs".into() }),
                vertex_buffers: vec![VertexLayout {
                    stride: 16,
                    step_mode: 1,
                    attrs: vec![VertexAttr { location: 0, format: 23, offset: 0 }],
                }],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Bgra8Unorm,
                    blend: Some(BlendState {
                        src_color: 1,
                        dst_color: 0,
                        op_color: 0,
                        src_alpha: 1,
                        dst_alpha: 0,
                        op_alpha: 0,
                    }),
                    write_mask: 0xF,
                }],
                depth: Some(DepthState {
                    format: TextureFormat::Depth24PlusStencil8,
                    depth_write: true,
                    depth_compare: 3,
                    stencil_front: StencilFaceState {
                        compare: compare::EQUAL,
                        fail_op: stencil_op::KEEP,
                        depth_fail_op: stencil_op::INCREMENT_CLAMP,
                        pass_op: stencil_op::REPLACE,
                    },
                    stencil_back: StencilFaceState {
                        compare: compare::NOT_EQUAL,
                        fail_op: stencil_op::INVERT,
                        depth_fail_op: stencil_op::ZERO,
                        pass_op: stencil_op::DECREMENT_WRAP,
                    },
                    stencil_read_mask: 0x0000_00FF,
                    stencil_write_mask: 0x0000_007F,
                }),
                topology: Topology::TriangleStrip,
                cull: 2,
                front_face: 1,
                sample_count: 1,
                label: "pipe".into(),
            },
        ),
        Cmd::CreateComputePipeline(
            9,
            ComputePipelineDesc { compute: ShaderRef { module: 5, entry: "k".into() }, label: "cp".into() },
        ),
        Cmd::CreateBindGroup(
            10,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 256 } },
                    BindEntry { binding: 1, resource: BindResource::Texture { id: 2 } },
                    BindEntry { binding: 2, resource: BindResource::Sampler { id: 3 } },
                ],
            },
        ),
        Cmd::CreateSurface(
            11,
            SurfaceDesc { width: 4, height: 4, format: TextureFormat::Bgra8Unorm, hlp_surface: 100 },
        ),
        Cmd::CreateFence(12),
        Cmd::Submit(CommandBuffer { encoder: every_encoder_op(), signal: Some((12, 7)) }),
        Cmd::WaitFence { id: 12, value: 7 },
        Cmd::Present { surface: 11, texture: 2 },
        Cmd::DestroyBindGroup(10),
        Cmd::DestroyPipeline(8),
        Cmd::DestroyShader(4),
        Cmd::DestroySampler(3),
        Cmd::DestroySurface(11),
        Cmd::DestroyTexture(2),
        Cmd::DestroyFence(12),
        Cmd::DestroyBuffer(1),
    ]
}

fn no_panic(bytes: &[u8]) -> hl_gpu::Result<Vec<Cmd>> {
    let owned = bytes.to_vec();
    match catch_unwind(move || decode_stream(&owned)) {
        Ok(r) => r,
        Err(_) => panic!("decode_stream PANICKED on {} bytes: {:02x?}", bytes.len(), bytes),
    }
}

// ---------------------------------------------------------------------------------------------------
// 1. every op / command round-trips, and truncating at EVERY prefix never panics
// ---------------------------------------------------------------------------------------------------

#[test]
fn every_command_and_op_value_round_trips() {
    let s = every_command();
    let bytes = encode_stream(&s);
    assert_eq!(decode_stream(&bytes).unwrap(), s, "the full tag/etag inventory round-trips by value");
}

#[test]
fn full_inventory_is_byte_stable() {
    // encode(decode(encode(x))) == encode(x): the decoder consumes exactly and re-encodes identically.
    let bytes = encode_stream(&every_command());
    let decoded = decode_stream(&bytes).unwrap();
    assert_eq!(encode_stream(&decoded), bytes, "decode∘encode is byte-stable across every op");
}

#[test]
fn truncating_each_op_at_every_prefix_never_panics() {
    // Each encoder op alone, wrapped in a Submit; truncate the bytes at every prefix — no panic, and the
    // untruncated form round-trips exactly.
    for op in every_encoder_op() {
        let cb = CommandBuffer { encoder: vec![op.clone()], signal: None };
        let bytes = encode_stream(&[Cmd::Submit(cb.clone())]);
        assert_eq!(decode_stream(&bytes).unwrap(), vec![Cmd::Submit(cb)], "op {op:?} round-trips");
        for cut in 0..bytes.len() {
            let _ = no_panic(&bytes[..cut]); // Err is fine; a panic is not.
        }
    }
}

#[test]
fn truncating_each_command_at_every_prefix_never_panics() {
    for cmd in every_command() {
        let bytes = encode_stream(&[cmd.clone()]);
        assert_eq!(decode_stream(&bytes).unwrap(), vec![cmd.clone()], "cmd round-trips");
        for cut in 0..bytes.len() {
            let _ = no_panic(&bytes[..cut]);
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// 2. byte-stability under adversarial mutation — the core invariant: ANY decodable bytes re-encode to
//    themselves. A failure here is a real normalization/desync bug.
// ---------------------------------------------------------------------------------------------------

/// Reproducible LCG byte source (no external RNG / time).
fn lcg(state: &mut u64) -> u8 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 56) as u8
}

#[test]
fn any_decodable_mutation_re_encodes_to_itself() {
    let base = encode_stream(&every_command());
    let mut state = 0xA5A5_1234_DEAD_0001u64;
    let mut decodable = 0u64;
    for _ in 0..40_000u32 {
        let mut bad = base.clone();
        // Apply 1..=3 single-byte writes at random positions.
        let muts = 1 + (lcg(&mut state) % 3) as usize;
        for _ in 0..muts {
            if bad.is_empty() {
                break;
            }
            let pos = (lcg(&mut state) as usize) % bad.len();
            bad[pos] = lcg(&mut state);
        }
        // Occasionally truncate to probe the framing boundary too.
        if lcg(&mut state) % 8 == 0 {
            let keep = (lcg(&mut state) as usize) % (bad.len() + 1);
            bad.truncate(keep);
        }
        if let Ok(cmds) = no_panic(&bad) {
            // THE INVARIANT: a stream the decoder accepted must re-encode to the exact same bytes it
            // consumed. decode_stream drains the whole input, so equality is total, not prefix.
            assert_eq!(
                encode_stream(&cmds),
                bad,
                "decode accepted bytes that re-encode differently (normalization/desync bug)"
            );
            decodable += 1;
        }
    }
    // Sanity: the corpus actually exercised the accept path, not only rejections.
    assert!(decodable > 0, "no mutation ever decoded — fuzz corpus is not exercising the accept path");
}

#[test]
fn random_bytes_never_panic_and_are_byte_stable_when_accepted() {
    let mut state = 0x0BAD_F00D_C0FF_EE00u64;
    for _ in 0..20_000u32 {
        let len = (lcg(&mut state) as usize) % 260;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            bytes.push(lcg(&mut state));
        }
        if let Ok(cmds) = no_panic(&bytes) {
            assert_eq!(encode_stream(&cmds), bytes, "accepted random bytes must be byte-stable");
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// 3. typed rejection of every malformed shape
// ---------------------------------------------------------------------------------------------------

#[test]
fn empty_stream_decodes_to_no_commands() {
    assert_eq!(decode_stream(&[]).unwrap(), Vec::<Cmd>::new());
}

#[test]
fn unknown_top_level_tag_is_bad_tag() {
    for bad_tag in [0u8, 22, 100, 255] {
        let err = decode_stream(&[bad_tag]).unwrap_err();
        assert!(
            matches!(&err, GpuError::Decode(m) if m.contains(&format!("bad command/encoder tag {bad_tag}"))),
            "tag {bad_tag} -> {err:?}"
        );
    }
}

#[test]
fn unknown_encoder_tag_inside_submit_is_bad_tag() {
    for bad_etag in [0u8, 23, 99, 255] {
        // Submit with one op whose etag byte is unknown.
        let mut e = Encoder::new();
        e.u8(tag::SUBMIT);
        e.u32(1); // encoder len
        e.u8(bad_etag); // the op tag
        let err = decode_stream(&e.into_vec()).unwrap_err();
        assert!(
            matches!(&err, GpuError::Decode(m) if m.contains(&format!("bad command/encoder tag {bad_etag}"))),
            "etag {bad_etag} -> {err:?}"
        );
    }
}

#[test]
fn out_of_range_enums_are_typed_bad_enum() {
    // Every wire enum's `from_u32` rejects an out-of-range value and accepts every in-range one, and
    // to_u32∘from_u32 is the identity on the valid domain.
    assert!(matches!(TextureFormat::from_u32(0), Err(GpuError::BadEnum { what: "TextureFormat", .. })));
    assert!(matches!(TextureFormat::from_u32(12), Err(GpuError::BadEnum { .. })));
    for v in 1..=11 {
        assert_eq!(TextureFormat::from_u32(v).unwrap().to_u32(), v);
    }
    assert!(matches!(TextureDim::from_u32(0), Err(GpuError::BadEnum { what: "TextureDim", .. })));
    assert!(matches!(TextureDim::from_u32(5), Err(GpuError::BadEnum { .. })));
    assert!(matches!(IndexFormat::from_u32(0), Err(GpuError::BadEnum { what: "IndexFormat", .. })));
    assert!(matches!(IndexFormat::from_u32(3), Err(GpuError::BadEnum { .. })));
    assert!(matches!(Topology::from_u32(5), Err(GpuError::BadEnum { what: "Topology", .. })));
    assert!(matches!(LoadOp::from_u32(3), Err(GpuError::BadEnum { what: "LoadOp", .. })));
    assert!(matches!(Filter::from_u32(2), Err(GpuError::BadEnum { what: "Filter", .. })));
    assert!(matches!(TextureAspect::from_u32(3), Err(GpuError::BadEnum { what: "TextureAspect", .. })));
    assert!(matches!(AddressMode::from_u32(3), Err(GpuError::BadEnum { what: "AddressMode", .. })));
}

#[test]
fn bad_enum_in_a_real_stream_is_rejected() {
    // A CreateTexture whose `dim` word is out of range must fail decode with a BadEnum context. Build the
    // bytes by hand so the exact field is corrupted.
    let mut e = Encoder::new();
    e.u8(tag::CREATE_TEXTURE);
    e.u32(2); // id
    e.u32(4); // width
    e.u32(4); // height
    e.u32(1); // depth
    e.u32(1); // mip_levels
    e.u32(1); // sample_count
    e.u32(99); // dim <-- out of range
    e.u32(1); // format
    e.u32(0); // usage
    e.str(""); // label
    let err = decode_stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("bad TextureDim enum value 99")),
        "{err:?}"
    );
}

#[test]
fn non_finite_render_floats_are_rejected_at_the_wire() {
    // The encoder does NOT reject NaN/±inf (a local producer bug), but the decoder MUST — a hostile
    // viewport/clear/depth float can never reach a backend. Each finite-float site is guarded.
    let bad_floats = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
    for bad in bad_floats {
        let sites = [
            Enc::SetViewport { x: bad, y: 0.0, w: 1.0, h: 1.0, min_depth: 0.0, max_depth: 1.0 },
            Enc::ClearRect { texture: 1, x: 0, y: 0, w: 1, h: 1, color: [bad, 0.0, 0.0, 1.0] },
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, bad, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment { texture: 1, load: LoadOp::Clear, clear_depth: bad, clear_stencil: 0 }),
            },
        ];
        for op in sites {
            let bytes = encode_stream(&[Cmd::Submit(CommandBuffer { encoder: vec![op.clone()], signal: None })]);
            let err = decode_stream(&bytes).unwrap_err();
            assert!(
                matches!(&err, GpuError::Decode(m) if m.contains("non-finite")),
                "op {op:?} with {bad} must reject non-finite: {err:?}"
            );
        }
    }
}

#[test]
fn non_canonical_bool_byte_is_rejected() {
    // A Submit with a single EndRenderPass op and a signal-present bool of 2 (neither 0 nor 1).
    let mut e = Encoder::new();
    e.u8(tag::SUBMIT);
    e.u32(1); // encoder len
    e.u8(etag::END_RENDER_PASS);
    e.u8(2); // signal-present bool <-- non-canonical
    let err = decode_stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("non-canonical boolean wire byte 2")),
        "{err:?}"
    );
}

#[test]
fn bad_bindresource_tag_is_typed_bad_enum() {
    // A CreateBindGroup entry whose resource discriminant byte is unknown (only 0/1/2 are valid).
    let mut e = Encoder::new();
    e.u8(tag::CREATE_BIND_GROUP);
    e.u32(1); // id
    e.u32(0); // set
    e.u32(1); // one entry
    e.u32(0); // binding
    e.u8(9); // resource tag <-- unknown
    let err = decode_stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("bad BindResource enum value 9")),
        "{err:?}"
    );
}

#[test]
fn bogus_length_prefix_is_short_buffer_not_a_giant_prealloc() {
    // WriteBuffer with a ~4-billion-byte data length but no body must fail cleanly (no multi-GB reserve).
    let mut e = Encoder::new();
    e.u8(tag::WRITE_BUFFER);
    e.u32(1); // id
    e.u64(0); // offset
    e.u32(0xFFFF_FFF0); // data length, nothing follows
    let err = decode_stream(&e.into_vec()).unwrap_err();
    assert!(matches!(&err, GpuError::Decode(m) if m.contains("short buffer")), "{err:?}");

    // Same for a CreateShader claiming ~4 billion words.
    let mut e = Encoder::new();
    e.u8(tag::CREATE_SHADER);
    e.u32(1); // id
    e.u32(0xFFFF_FFF0); // word count, no words follow
    let err = decode_stream(&e.into_vec()).unwrap_err();
    assert!(matches!(&err, GpuError::Decode(m) if m.contains("short buffer")), "{err:?}");
}

#[test]
fn framed_command_rejects_trailing_bytes() {
    let cmd = Cmd::CreateFence(1);
    let mut e = Encoder::new();
    e.frame(|inner| {
        cmd.encode(inner);
        inner.u8(0xEE); // trailing garbage inside the frame body
    });
    let framed = e.into_vec();
    let mut d = Decoder::new(&framed);
    assert_eq!(Cmd::decode_frame(&mut d), Err(GpuError::TrailingBytes));
}

#[test]
fn boundary_field_values_round_trip() {
    // Extreme-but-valid field values: u32/u64 saturation, i32 min/max base_vertex, empty + long strings,
    // empty vectors, and finite-float extremes.
    let big_source: String = "x".repeat(4096);
    let cmds = vec![
        Cmd::CreateBuffer(u32::MAX, BufferDesc { size: u64::MAX, usage: u32::MAX, label: String::new() }),
        Cmd::WriteBuffer { id: 0, offset: u64::MAX, data: vec![] },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::DrawIndexed {
                    index_count: u32::MAX,
                    instance_count: u32::MAX,
                    first_index: u32::MAX,
                    base_vertex: i32::MIN,
                    first_instance: u32::MAX,
                },
                Enc::DrawIndexed {
                    index_count: 0,
                    instance_count: 0,
                    first_index: 0,
                    base_vertex: i32::MAX,
                    first_instance: 0,
                },
                Enc::SetVertexBuffer { slot: u32::MAX, buffer: u32::MAX, offset: u64::MAX },
                Enc::SetViewport {
                    x: f32::MIN,
                    y: f32::MAX,
                    w: f32::MIN_POSITIVE,
                    h: -0.0,
                    min_depth: 0.0,
                    max_depth: 1.0,
                },
                Enc::FillBuffer { buffer: 0, offset: u64::MAX, size: u64::MAX, value: u32::MAX },
            ],
            signal: Some((u32::MAX, u64::MAX)),
        }),
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: GlslDescriptor { stage: glsl_stage::VERTEX, entry: String::new(), source: big_source }
                .to_words(),
        },
    ];
    let bytes = encode_stream(&cmds);
    assert_eq!(decode_stream(&bytes).unwrap(), cmds, "boundary values survive the wire unchanged");
    assert_eq!(encode_stream(&decode_stream(&bytes).unwrap()), bytes, "and are byte-stable");
}

#[test]
fn wire_version_and_magics_are_pinned() {
    // A version bump or a magic change must be a deliberate, reviewed edit (matches the frozen goldens).
    // Bumped 7 → 8 when MSAA added `RenderPipelineDesc.sample_count` (appended after `front_face`).
    assert_eq!(WIRE_VERSION, 8);
    assert_eq!(SPIRV_MAGIC, 0x0723_0203);
    assert_eq!(KERNEL_MAGIC, 0xDD6B_0001);
    assert_eq!(GLSL_MAGIC, 0xDD67_0001);
    // The three magics are mutually distinct so payload classification is unambiguous.
    assert_ne!(SPIRV_MAGIC, KERNEL_MAGIC);
    assert_ne!(KERNEL_MAGIC, GLSL_MAGIC);
    assert_ne!(SPIRV_MAGIC, GLSL_MAGIC);
}

// ---------------------------------------------------------------------------------------------------
// 4. the neutral kernel/GLSL descriptor decoders (executor-facing) reject malformed payloads
// ---------------------------------------------------------------------------------------------------

#[test]
fn kernel_and_glsl_descriptor_from_words_are_robust() {
    // Wrong / missing magic -> None (not this kind).
    assert!(KernelDescriptor::from_words(&[SPIRV_MAGIC, 0]).is_none());
    assert!(KernelDescriptor::from_words(&[]).is_none());
    assert!(GlslDescriptor::from_words(&[KERNEL_MAGIC, 0]).is_none());
    assert!(GlslDescriptor::from_words(&[GLSL_MAGIC]).is_none()); // < 2 words

    // Declared byte length exceeds the payload -> a typed truncation error, never a panic/OOB.
    match KernelDescriptor::from_words(&[KERNEL_MAGIC, 0xFFFF_FFFF, 1, 2]) {
        Some(Err(GpuError::Kernel(_))) => {}
        other => panic!("kernel truncation must be a typed Kernel error, got {other:?}"),
    }
    match GlslDescriptor::from_words(&[GLSL_MAGIC, 0xFFFF_FFFF, 1, 2]) {
        Some(Err(GpuError::Kernel(_))) => {}
        other => panic!("glsl truncation must be a typed error, got {other:?}"),
    }

    // A real descriptor survives the words round-trip.
    let kd = KernelDescriptor { ptx: "mov;".into(), entry: "e".into(), block: [1, 2, 3] };
    assert_eq!(KernelDescriptor::from_words(&kd.to_words()).unwrap().unwrap(), kd);
    let gd = GlslDescriptor { stage: glsl_stage::COMPUTE, entry: "c".into(), source: "void main(){}".into() };
    assert_eq!(GlslDescriptor::from_words(&gd.to_words()).unwrap().unwrap(), gd);

    // A GLSL payload with a truncated INNER body (byte_len fits the words, but the framed strings run past
    // the declared length) yields a typed error, never a panic.
    let mut words = gd.to_words();
    if let Some(last) = words.last_mut() {
        *last = 0xFFFF_FFFF; // corrupt the tail
    }
    let _ = catch_unwind(move || {
        let _ = GlslDescriptor::from_words(&words);
    })
    .expect("GLSL descriptor decode must not panic on a corrupted tail");
}
