//! Real-Metal executor coverage for the Phase-3 texture-to-texture copy + scaled blit IR ops. Mirrors the
//! software oracle's `copy_texture_to_texture_moves_the_requested_region` / `blit_nearest_scales_up_...`
//! behavioral tests: drive the SAME IR through `MetalBackend` on a live device and read the destination
//! back, so the bespoke Metal executor is proven to implement the op (not just the CPU oracle). Skips
//! cleanly when no Metal device is present (CI without a GPU).

#[cfg(target_os = "macos")]
mod macos {
    use hl_display::metal::MetalCtx;
    use hl_display::metal_backend::MetalBackend;
    use hl_gpu::backend::GpuBackend;
    use hl_gpu::id::{BufferId, TextureId};
    use hl_gpu::ir::{
        buffer_usage, texture_usage, BufferDesc, CommandBuffer, Enc, Extent3d, Filter, Origin3d,
        TextureDesc, TextureDim, TextureFormat, TextureSubresource,
    };
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{MTLOrigin, MTLRegion, MTLSize, MTLTexture};

    fn tex_desc(w: u32, h: u32, usage: u32) -> TextureDesc {
        TextureDesc {
            width: w,
            height: h,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage,
            label: String::new(),
        }
    }

    /// Create a `w`x`h` texture `id` and upload `pattern` (tight RGBA rows) into it via a staging buffer.
    fn seed(be: &mut MetalBackend, id: u32, w: u32, h: u32, usage: u32, pattern: &[u8]) {
        be.create_texture(TextureId(id), &tex_desc(w, h, usage | texture_usage::COPY_DST)).unwrap();
        let stage = 900 + id;
        be.create_buffer(
            BufferId(stage),
            &BufferDesc { size: pattern.len() as u64, usage: buffer_usage::COPY_SRC, label: String::new() },
        )
        .unwrap();
        be.write_buffer(BufferId(stage), 0, pattern).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture { src: stage, src_offset: 0, bytes_per_row: w * 4, dst: id, mip: 0, width: w, height: h }],
            signal: None,
        })
        .unwrap();
    }

    fn read_rgba(tex: &ProtocolObject<dyn MTLTexture>, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; (w * h * 4) as usize];
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize { width: w as usize, height: h as usize, depth: 1 },
        };
        unsafe {
            tex.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(out.as_mut_ptr() as *mut _).unwrap(),
                (w * 4) as usize,
                region,
                0,
            );
        }
        out
    }

    fn sub0() -> TextureSubresource {
        TextureSubresource::base()
    }

    fn near(got: [u8; 4], want: [u8; 4]) -> bool {
        got.iter().zip(want).all(|(g, w)| (*g as i16 - w as i16).abs() <= 3)
    }

    #[test]
    fn copy_texture_to_texture_moves_the_requested_region() {
        let Some(ctx) = MetalCtx::new() else {
            eprintln!("skipping: no Metal device");
            return;
        };
        let mut be = MetalBackend::new(&ctx);
        // src 4x4 where texel (x,y) = [x*10, y*10, 0, 255].
        let mut src = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                src.extend_from_slice(&[x * 10, y * 10, 0, 255]);
            }
        }
        seed(&mut be, 1, 4, 4, texture_usage::SAMPLED | texture_usage::COPY_SRC, &src);
        be.create_texture(TextureId(2), &tex_desc(4, 4, texture_usage::COPY_DST | texture_usage::RENDER_TARGET)).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![Enc::CopyTextureToTexture {
                src: 1,
                src_sub: sub0(),
                src_origin: Origin3d { x: 1, y: 1, z: 0 },
                dst: 2,
                dst_sub: sub0(),
                dst_origin: Origin3d::default(),
                extent: Extent3d { width: 2, height: 2, depth: 1 },
            }],
            signal: None,
        })
        .unwrap();
        let out = read_rgba(be.texture(2).expect("dst texture"), 4, 4);
        let px = |x: usize, y: usize| { let o = (y * 4 + x) * 4; [out[o], out[o + 1], out[o + 2], out[o + 3]] };
        assert!(near(px(0, 0), [10, 10, 0, 255]), "got {:?}", px(0, 0));
        assert!(near(px(1, 0), [20, 10, 0, 255]), "got {:?}", px(1, 0));
        assert!(near(px(1, 1), [20, 20, 0, 255]), "got {:?}", px(1, 1));
    }

    #[test]
    fn blit_nearest_scales_up_by_block_replication() {
        let Some(ctx) = MetalCtx::new() else {
            eprintln!("skipping: no Metal device");
            return;
        };
        let mut be = MetalBackend::new(&ctx);
        // 2x2 src: red, green / blue, white.
        let src: [u8; 16] = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
        seed(&mut be, 1, 2, 2, texture_usage::SAMPLED | texture_usage::COPY_SRC, &src);
        be.create_texture(TextureId(3), &tex_desc(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_DST)).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![Enc::BlitTexture {
                src: 1,
                src_sub: sub0(),
                src_origin: Origin3d::default(),
                src_extent: Extent3d { width: 2, height: 2, depth: 1 },
                dst: 3,
                dst_sub: sub0(),
                dst_origin: Origin3d::default(),
                dst_extent: Extent3d { width: 4, height: 4, depth: 1 },
                filter: Filter::Nearest,
            }],
            signal: None,
        })
        .unwrap();
        let out = read_rgba(be.texture(3).expect("dst texture"), 4, 4);
        let px = |x: usize, y: usize| { let o = (y * 4 + x) * 4; [out[o], out[o + 1], out[o + 2], out[o + 3]] };
        assert!(near(px(0, 0), [255, 0, 0, 255]), "top-left got {:?}", px(0, 0));
        assert!(near(px(1, 1), [255, 0, 0, 255]), "top-left block got {:?}", px(1, 1));
        assert!(near(px(2, 0), [0, 255, 0, 255]), "top-right got {:?}", px(2, 0));
        assert!(near(px(0, 2), [0, 0, 255, 255]), "bottom-left got {:?}", px(0, 2));
        assert!(near(px(3, 3), [255, 255, 255, 255]), "bottom-right got {:?}", px(3, 3));
    }

    #[test]
    fn copy_texture_to_texture_out_of_bounds_is_rejected_not_ub() {
        let Some(ctx) = MetalCtx::new() else {
            eprintln!("skipping: no Metal device");
            return;
        };
        let mut be = MetalBackend::new(&ctx);
        be.create_texture(TextureId(1), &tex_desc(4, 4, texture_usage::SAMPLED | texture_usage::COPY_SRC)).unwrap();
        be.create_texture(TextureId(2), &tex_desc(4, 4, texture_usage::COPY_DST | texture_usage::RENDER_TARGET)).unwrap();
        let r = be.submit(&CommandBuffer {
            encoder: vec![Enc::CopyTextureToTexture {
                src: 1,
                src_sub: sub0(),
                src_origin: Origin3d { x: 3, y: 3, z: 0 },
                dst: 2,
                dst_sub: sub0(),
                dst_origin: Origin3d::default(),
                extent: Extent3d { width: 4, height: 4, depth: 1 },
            }],
            signal: None,
        });
        assert!(matches!(r, Err(hl_gpu::GpuError::OutOfBounds)));
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn metal_texture_copy_blit_tests_require_macos() {
    eprintln!("Metal texture copy/blit tests are macOS-only");
}
