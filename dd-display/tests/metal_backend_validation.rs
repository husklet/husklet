//! Behavior tests for the Metal backend's validation / error-propagation, pinning the fixes for the
//! Metal findings in docs/bugs/gpu-display-sentry.md. These assert typed errors (not pixels), so they
//! are independent of GPU-specific render output.

#[cfg(target_os = "macos")]
mod macos {
    use dd_display::metal::MetalCtx;
    use dd_display::metal_backend::MetalBackend;
    use dd_gpu::backend::GpuBackend;
    use dd_gpu::id::{BindGroupId, BufferId, PipelineId, SamplerId, TextureId};
    use dd_gpu::ir::{
        buffer_usage, BindEntry, BindGroupDesc, BindResource, BufferDesc, CommandBuffer, Enc,
    };
    use dd_gpu::GpuError;

    fn ctx() -> Option<MetalCtx> {
        match MetalCtx::new() {
            Some(c) => Some(c),
            None => {
                eprintln!("skipping: no Metal device");
                None
            }
        }
    }

    #[test]
    fn write_buffer_out_of_range_is_error_not_skipped() {
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        be.create_buffer(BufferId(1), &BufferDesc { size: 16, usage: buffer_usage::COPY_DST, label: String::new() }).unwrap();
        assert_eq!(be.write_buffer(BufferId(1), 100, &[0u8; 8]), Err(GpuError::OutOfBounds));
        // a wrapping offset is also a clean error, not a panic
        assert_eq!(be.write_buffer(BufferId(1), u64::MAX, &[0u8; 4]), Err(GpuError::OutOfBounds));
    }

    #[test]
    fn bind_group_missing_resources_are_rejected() {
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        // missing texture
        let r = be.create_bind_group(BindGroupId(1), &BindGroupDesc {
            set: 0,
            entries: vec![BindEntry { binding: 0, resource: BindResource::Texture { id: 99 } }],
        });
        assert_eq!(r, Err(GpuError::UnknownId { kind: "texture", id: 99 }));
        // missing buffer
        let r = be.create_bind_group(BindGroupId(2), &BindGroupDesc {
            set: 0,
            entries: vec![BindEntry { binding: 1, resource: BindResource::Buffer { id: 77, offset: 0, size: 0 } }],
        });
        assert_eq!(r, Err(GpuError::UnknownId { kind: "buffer", id: 77 }));
        // missing sampler
        let r = be.create_bind_group(BindGroupId(3), &BindGroupDesc {
            set: 0,
            entries: vec![BindEntry { binding: 0, resource: BindResource::Sampler { id: 55 } }],
        });
        assert_eq!(r, Err(GpuError::UnknownId { kind: "sampler", id: 55 }));
    }

    #[test]
    fn dispatch_without_a_compute_pipeline_is_an_error() {
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        // A dispatch with no compute pipeline bound is a malformed stream — it must error (not silently
        // no-op), so the executor acks failure. (The Metal executor now HAS a compute path — see the
        // positive vecadd test below — so the failure here is the missing pipeline, not "compute
        // unsupported".)
        let r = be.submit(&CommandBuffer {
            encoder: vec![Enc::BeginComputePass, Enc::Dispatch { x: 1, y: 1, z: 1 }, Enc::EndComputePass],
            signal: None,
        });
        assert!(matches!(r, Err(GpuError::Unsupported(_))), "dispatch must error, got {r:?}");
    }

    #[test]
    fn compute_pipeline_with_non_msl_shader_routes_to_wgpu() {
        use dd_gpu::id::ShaderId;
        use dd_gpu::ir::{ComputePipelineDesc, ShaderRef};
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        // A SPIR-V/PTX compute module can't be transpiled in-process by the bespoke Metal executor. Its
        // `create_shader` leaves no MSL library, so `create_compute_pipeline` must reject with the EXPLICIT
        // routing error — a documented decision (steer compute to DD_GPU_BACKEND=wgpu), never a silent stub.
        // Real SPIR-V words (magic 0x07230203) — decidedly not the MSL-bytes packing the shim uses.
        be.create_shader(ShaderId(20), dd_gpu::ir::ShaderPayloadKind::SpirV, &[0x0723_0203, 0, 0, 0, 0]).unwrap();
        let r = be.create_compute_pipeline(
            PipelineId(30),
            &ComputePipelineDesc { compute: ShaderRef { module: 20, entry: "main".into() }, label: String::new() },
        );
        match r {
            Err(GpuError::Unsupported(msg)) => assert!(
                msg.contains("wgpu"),
                "compute-routing error must name the wgpu backend, got {msg:?}"
            ),
            other => panic!("non-MSL compute pipeline must return the wgpu-routing error, got {other:?}"),
        }
    }

    /// Numeric proof of REAL compute on the DEFAULT (bespoke Metal) executor: an MSL `vecadd` kernel over
    /// N elements dispatched through `MTLComputeCommandEncoder`, with `read_buffer` readback proving
    /// `c[i] == a[i] + b[i]`. This closes the audit's Phase-3 gap "compute needs a deliberate Metal
    /// implementation" for the Metal-native (MSL) shader ABI — the same ABI the render path consumes.
    #[test]
    fn msl_compute_vecadd_reads_back_sum_on_metal() {
        use dd_gpu::id::ShaderId;
        use dd_gpu::ir::{ComputePipelineDesc, ShaderRef};
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);

        const N: usize = 256;
        // Contract: Dispatch{N,1,1} launches N single-thread threadgroups, so `thread_position_in_grid.x`
        // is the global element index. The kernel guards against N to stay safe if N isn't a grid multiple.
        const MSL: &str = "#include <metal_stdlib>\nusing namespace metal;\nkernel void vecadd(device const float* a [[buffer(0)]], device const float* b [[buffer(1)]], device float* c [[buffer(2)]], uint gid [[thread_position_in_grid]]) { if (gid < 256u) { c[gid] = a[gid] + b[gid]; } }\n";

        // pack MSL into IR shader words (word[0]=len, rest = bytes 4/word LE) — the shim/msl_from_words ABI.
        let words = {
            let mut w = vec![MSL.len() as u32];
            let bytes = MSL.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let mut word = [0u8; 4];
                for k in 0..4 { if i + k < bytes.len() { word[k] = bytes[i + k]; } }
                w.push(u32::from_le_bytes(word));
                i += 4;
            }
            w
        };

        let mut a = vec![0u8; N * 4];
        let mut b = vec![0u8; N * 4];
        for i in 0..N {
            a[i * 4..i * 4 + 4].copy_from_slice(&(i as f32).to_le_bytes());
            b[i * 4..i * 4 + 4].copy_from_slice(&((2 * i) as f32).to_le_bytes());
        }
        let sz = (N * 4) as u64;
        be.create_buffer(BufferId(1), &BufferDesc { size: sz, usage: buffer_usage::STORAGE, label: String::new() }).unwrap();
        be.create_buffer(BufferId(2), &BufferDesc { size: sz, usage: buffer_usage::STORAGE, label: String::new() }).unwrap();
        be.create_buffer(BufferId(3), &BufferDesc { size: sz, usage: buffer_usage::STORAGE, label: String::new() }).unwrap();
        be.write_buffer(BufferId(1), 0, &a).unwrap();
        be.write_buffer(BufferId(2), 0, &b).unwrap();
        be.create_shader(ShaderId(20), dd_gpu::ir::ShaderPayloadKind::LegacyMsl, &words).unwrap();
        be.create_compute_pipeline(
            PipelineId(30),
            &ComputePipelineDesc { compute: ShaderRef { module: 20, entry: "vecadd".into() }, label: String::new() },
        ).expect("MSL compute pipeline must build on Metal");
        be.create_bind_group(BindGroupId(40), &BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: sz } },
                BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: sz } },
                BindEntry { binding: 2, resource: BindResource::Buffer { id: 3, offset: 0, size: sz } },
            ],
        }).unwrap();

        be.submit(&CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(30),
                Enc::SetBindGroup { index: 0, group: 40 },
                Enc::Dispatch { x: N as u32, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: None,
        }).expect("compute submit");

        let mut c = vec![0u8; N * 4];
        be.read_buffer(BufferId(3), 0, &mut c).expect("read_buffer c");
        for i in 0..N {
            let got = f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap());
            let want = i as f32 + (2 * i) as f32;
            assert_eq!(got, want, "c[{i}] = {got}, want {want} (a+b)");
        }
        eprintln!("msl_compute_vecadd: c == a + b for all {N} elements (real Metal MTLComputeCommandEncoder)");
    }

    #[test]
    fn copy_buffer_to_buffer_and_readback_on_metal() {
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        let src: Vec<u8> = (0..64u8).collect();
        be.create_buffer(BufferId(1), &BufferDesc { size: 64, usage: buffer_usage::COPY_SRC, label: String::new() }).unwrap();
        be.create_buffer(BufferId(2), &BufferDesc { size: 64, usage: buffer_usage::COPY_DST, label: String::new() }).unwrap();
        be.write_buffer(BufferId(1), 0, &src).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 2, dst_offset: 0, size: 64 }],
            signal: None,
        }).expect("b2b copy");
        let mut out = vec![0u8; 64];
        be.read_buffer(BufferId(2), 0, &mut out).unwrap();
        assert_eq!(out, src, "CopyBufferToBuffer must reproduce the source bytes");
        // read_buffer bounds are enforced (untrusted IR).
        assert_eq!(be.read_buffer(BufferId(2), 40, &mut [0u8; 40]), Err(GpuError::OutOfBounds));
    }

    #[test]
    fn destroy_pipeline_and_bind_group_are_supported() {
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        // A bind group can be created and destroyed (cleanup parity with checked backends).
        be.create_buffer(BufferId(1), &BufferDesc { size: 16, usage: buffer_usage::UNIFORM, label: String::new() }).unwrap();
        be.create_bind_group(BindGroupId(1), &BindGroupDesc {
            set: 0,
            entries: vec![BindEntry { binding: 1, resource: BindResource::Buffer { id: 1, offset: 0, size: 16 } }],
        }).unwrap();
        assert!(be.destroy_bind_group(BindGroupId(1)).is_ok());
        // destroy_pipeline is implemented (not the trait's Unsupported default).
        assert!(be.destroy_pipeline(PipelineId(123)).is_ok());
        // sampler create applies all descriptor fields without error.
        use dd_gpu::ir::{AddressMode, Filter, SamplerDesc};
        be.create_sampler(SamplerId(9), &SamplerDesc {
            min_filter: Filter::Linear,
            mag_filter: Filter::Linear,
            mip_filter: Filter::Linear,
            address_u: AddressMode::Repeat,
            address_v: AddressMode::MirrorRepeat,
            address_w: AddressMode::ClampToEdge,
        }).unwrap();
        let _ = TextureId(0); // silence unused import in some cfgs
    }

    /// The executor reserves texture id 1 for the presented surface (`set_render_target`). A guest that
    /// creates a texture with that same id must get its OWN distinct texture, never a silent alias of the
    /// present render target. Before the fix, `create_texture` for the reserved id was a no-op, so id 1
    /// stayed the present RT and the guest's texture aliased the presented surface (corrupting output).
    #[test]
    fn guest_create_texture_for_reserved_present_id_is_not_swallowed() {
        use dd_gpu::ir::{texture_usage, TextureDesc, TextureDim, TextureFormat};
        use objc2_metal::MTLTexture;
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        // Register a 4x4 present target at the reserved id 1; it resolves at that id.
        let present = ctx.new_bgra_texture(4, 4);
        be.set_render_target(1, present.clone());
        assert_eq!(be.texture(1).map(|t| t.width()), Some(4), "present target resolves at reserved id 1");

        // A guest create for the SAME id must materialize a distinct 16x16 texture, not a present-target no-op.
        be.create_texture(TextureId(1), &TextureDesc {
            width: 16,
            height: 16,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Bgra8Unorm,
            usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET,
            label: String::new(),
        }).unwrap();

        // id 1 now resolves to the guest's own 16x16 texture (guest wins its own id) — proving the create
        // was not swallowed as an alias of the 4x4 present RT.
        assert_eq!(
            be.texture(1).map(|t| t.width()),
            Some(16),
            "guest create_texture(1) must be a distinct texture, not an alias of the present RT"
        );
        // The present target itself is untouched in its own namespace.
        assert_eq!(present.width(), 4);
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn metal_backend_validation_requires_macos() {
    eprintln!("Metal backend validation tests are macOS-only");
}
