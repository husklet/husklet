//! Real-Metal executor coverage for multisample resolve (`Enc::ResolveTexture`). Drives the SAME resolve
//! IR the software oracle averages through `MetalBackend` on a live device: the resolved destination reads
//! back the per-sample average, proving genuine multisample averaging on the bespoke Metal executor. The
//! MS source is seeded with known distinct per-sample values (per-sample shading via
//! `MetalBackend::seed_multisample_uniform`), and the resolved pixels are cross-checked against the
//! software oracle fed the identical per-sample data. Skips when no Metal device is present.

#[cfg(target_os = "macos")]
mod macos {
    use hl_display::metal::MetalCtx;
    use hl_display::metal_backend::MetalBackend;
    use hl_gpu::backend::GpuBackend;
    use hl_gpu::id::{BufferId, TextureId};
    use hl_gpu::ir::{
        buffer_usage, texture_usage, BufferDesc, CommandBuffer, Enc, Extent3d, Origin3d, TextureDesc,
        TextureDim, TextureFormat, TextureSubresource,
    };
    use hl_gpu::software::SoftwareBackend;
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{MTLOrigin, MTLRegion, MTLSize, MTLTexture};

    // Four distinct per-sample RGBA8 colors; their arithmetic mean is exactly [60,80,100,120].
    const S0: [u8; 4] = [0, 20, 40, 60];
    const S1: [u8; 4] = [40, 60, 80, 100];
    const S2: [u8; 4] = [80, 100, 120, 140];
    const S3: [u8; 4] = [120, 140, 160, 180];
    const AVG: [u8; 4] = [60, 80, 100, 120];

    fn ms_desc(w: u32, h: u32, samples: u32) -> TextureDesc {
        TextureDesc {
            width: w, height: h, depth: 1, mip_levels: 1, sample_count: samples,
            dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC, label: String::new(),
        }
    }
    fn dst_desc(w: u32, h: u32) -> TextureDesc {
        TextureDesc {
            width: w, height: h, depth: 1, mip_levels: 1, sample_count: 1,
            dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST,
            label: String::new(),
        }
    }
    fn f(c: [u8; 4]) -> [f32; 4] {
        [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
    }
    fn near(got: [u8; 4], want: [u8; 4]) -> bool {
        got.iter().zip(want).all(|(g, w)| (*g as i16 - w as i16).abs() <= 2)
    }
    fn texel(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * w + x) * 4) as usize;
        [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
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

    #[test]
    fn metal_resolve_averages_samples_into_a_subregion_matching_the_software_oracle() {
        let Some(ctx) = MetalCtx::new() else {
            eprintln!("skipping: no Metal device");
            return;
        };
        let mut be = MetalBackend::new(&ctx);
        let (w, h) = (4u32, 4u32);
        let (src, dst) = (1u32, 2u32);

        // MS source + dst; seed the dst with a background so we can prove the resolve touches ONLY its region.
        be.create_texture(TextureId(src), &ms_desc(w, h, 4)).unwrap();
        be.create_texture(TextureId(dst), &dst_desc(w, h)).unwrap();
        let bg = [10u8, 10, 10, 255];
        let mut bg_bytes = Vec::new();
        for _ in 0..w * h { bg_bytes.extend_from_slice(&bg); }
        be.create_buffer(BufferId(900), &BufferDesc { size: bg_bytes.len() as u64, usage: buffer_usage::COPY_SRC, label: String::new() }).unwrap();
        be.write_buffer(BufferId(900), 0, &bg_bytes).unwrap();
        be.submit(&CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture { src: 900, src_offset: 0, bytes_per_row: w * 4, dst, mip: 0, width: w, height: h }],
            signal: None,
        }).unwrap();

        // Seed distinct per-sample data into the MS source.
        be.seed_multisample_uniform(src, &[f(S0), f(S1), f(S2), f(S3)]).unwrap();

        // Resolve a 2x2 region from src origin (2,2) into dst origin (1,1) — exercises the offset mapping.
        let resolve = Enc::ResolveTexture {
            src, src_sub: TextureSubresource::base(), src_origin: Origin3d { x: 2, y: 2, z: 0 },
            dst, dst_sub: TextureSubresource::base(), dst_origin: Origin3d { x: 1, y: 1, z: 0 },
            extent: Extent3d { width: 2, height: 2, depth: 1 },
        };
        // Real >1-sample MSAA resolve MUST run on Metal (the RESOLVE_MSL averaging pass) and produce the
        // per-sample mean — not skip, not Unsupported. The metal_backend arm averages the samples.
        be.submit(&CommandBuffer { encoder: vec![resolve.clone()], signal: None })
            .expect("Metal multisample resolve must succeed (RESOLVE_MSL averaging pass)");

        let out = read_rgba(be.texture(dst).expect("dst texture"), w, h);
        for y in 0..h {
            for x in 0..w {
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                let want = if inside { AVG } else { bg };
                assert!(
                    near(texel(&out, w, x, y), want),
                    "metal resolve texel ({x},{y}) = {:?}, want {want:?}",
                    texel(&out, w, x, y)
                );
            }
        }

        // Parity vs the software oracle: identical per-sample data + resolve region.
        let mut sw = SoftwareBackend::new();
        sw.create_texture(TextureId(src), &ms_desc(w, h, 4)).unwrap();
        sw.create_texture(TextureId(dst), &dst_desc(w, h)).unwrap();
        let mut samples = Vec::new();
        for _ in 0..w * h { for s in [S0, S1, S2, S3] { samples.extend_from_slice(&s); } }
        sw.write_texture_samples(TextureId(src), &samples).unwrap();
        hl_gpu::replay::replay(&mut sw, &[hl_gpu::ir::Cmd::Submit(CommandBuffer { encoder: vec![resolve], signal: None })]).unwrap();
        let mut sw_full = vec![0u8; (w * h * 4) as usize];
        sw.read_texture(TextureId(dst), &mut sw_full).unwrap();
        let sw_avg = texel(&sw_full, w, 1, 1);
        assert!(near(sw_avg, AVG), "software oracle resolve = {sw_avg:?}");
        assert!(near(texel(&out, w, 1, 1), sw_avg), "metal {:?} != software oracle {:?}", texel(&out, w, 1, 1), sw_avg);
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn metal_resolve_tests_require_macos() {
    eprintln!("Metal resolve tests are macOS-only");
}
