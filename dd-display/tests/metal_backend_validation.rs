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
    fn dispatch_in_render_executor_is_unsupported_error() {
        let Some(ctx) = ctx() else { return };
        let mut be = MetalBackend::new(&ctx);
        // The render-only executor must not silently drop a compute dispatch — it must error so the
        // executor acks failure.
        let r = be.submit(&CommandBuffer {
            encoder: vec![Enc::BeginComputePass, Enc::Dispatch { x: 1, y: 1, z: 1 }, Enc::EndComputePass],
            signal: None,
        });
        assert!(matches!(r, Err(GpuError::Unsupported(_))), "dispatch must error, got {r:?}");
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
