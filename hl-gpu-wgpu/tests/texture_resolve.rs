//! wgpu executor coverage for multisample resolve (`Enc::ResolveTexture`) on a real Metal device.
//!
//! The SAME resolve IR the software oracle averages flows through `WgpuBackend`, and the resolved
//! destination reads back the per-sample average — proving genuine multisample averaging on the hardware
//! executor (not a truncated copy). We seed the MS source with known, distinct per-sample values (the
//! only portable way to control individual hardware samples is per-sample shading — see
//! `WgpuBackend::seed_multisample_uniform`), then cross-check the resolved pixels against the software
//! oracle fed the identical per-sample data. Skips when no Metal adapter is present. macOS only.

#![cfg(target_os = "macos")]

use hl_gpu::backend::GpuBackend;
use hl_gpu::id::TextureId;
use hl_gpu::ir::*;
use hl_gpu::software::SoftwareBackend;
use hl_gpu_wgpu::WgpuBackend;

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
        // RENDER_TARGET so the MS texture can be a color attachment (wgpu forces MS usage to
        // RENDER_ATTACHMENT|TEXTURE_BINDING regardless); COPY_SRC so the software oracle accepts it as a
        // resolve source (the oracle validates transfer usage on the sampled plane).
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
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

#[test]
fn wgpu_resolve_averages_samples_into_a_subregion_matching_the_software_oracle() {
    let Ok(mut be) = WgpuBackend::new() else {
        eprintln!("skipping: no Metal adapter");
        return;
    };
    let (w, h) = (4u32, 4u32);
    let (src, dst) = (1u32, 2u32);

    // --- background for the dst (so we can prove the resolve touches ONLY its region) ---
    let bg = [10u8, 10, 10, 255];
    let mut bg_bytes = Vec::new();
    for _ in 0..w * h { bg_bytes.extend_from_slice(&bg); }
    let stage = 900u32;

    let setup = vec![
        Cmd::CreateTexture(src, ms_desc(w, h, 4)),
        Cmd::CreateTexture(dst, dst_desc(w, h)),
        Cmd::CreateBuffer(stage, BufferDesc { size: bg_bytes.len() as u64, usage: buffer_usage::COPY_SRC, label: String::new() }),
        Cmd::WriteBuffer { id: stage, offset: 0, data: bg_bytes.clone() },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture { src: stage, src_offset: 0, bytes_per_row: w * 4, dst, mip: 0, width: w, height: h }],
            signal: None,
        }),
    ];
    // Materialize the MS source + dst background BEFORE seeding per-sample data (seed needs the texture).
    hl_gpu::replay::replay(&mut be, &setup).unwrap();
    be.seed_multisample_uniform(src, &[f(S0), f(S1), f(S2), f(S3)]).unwrap();

    // Resolve a 2x2 region from src origin (2,2) into dst origin (1,1) — exercises the offset mapping.
    let resolve = Enc::ResolveTexture {
        src, src_sub: TextureSubresource::base(), src_origin: Origin3d { x: 2, y: 2, z: 0 },
        dst, dst_sub: TextureSubresource::base(), dst_origin: Origin3d { x: 1, y: 1, z: 0 },
        extent: Extent3d { width: 2, height: 2, depth: 1 },
    };
    hl_gpu::replay::replay(&mut be, &[Cmd::Submit(CommandBuffer { encoder: vec![resolve.clone()], signal: None })]).unwrap();

    let out = be.read_target(dst).unwrap();
    // The resolved 2x2 region equals the per-sample average; every other texel keeps the background.
    for y in 0..h {
        for x in 0..w {
            let inside = (1..3).contains(&x) && (1..3).contains(&y);
            let want = if inside { AVG } else { bg };
            assert!(
                near(texel(&out, w, x, y), want),
                "wgpu resolve texel ({x},{y}) = {:?}, want {want:?}",
                texel(&out, w, x, y)
            );
        }
    }

    // --- parity vs the software oracle: same per-sample data, same resolve region ---
    let mut sw = SoftwareBackend::new();
    sw.create_texture(TextureId(src), &ms_desc(w, h, 4)).unwrap();
    sw.create_texture(TextureId(dst), &dst_desc(w, h)).unwrap();
    // Seed every source texel with the identical [S0,S1,S2,S3] per-sample pattern (texel-, sample-, channel-major).
    let mut samples = Vec::new();
    for _ in 0..w * h { for s in [S0, S1, S2, S3] { samples.extend_from_slice(&s); } }
    sw.write_texture_samples(TextureId(src), &samples).unwrap();
    hl_gpu::replay::replay(&mut sw, &[Cmd::Submit(CommandBuffer { encoder: vec![resolve], signal: None })]).unwrap();
    // Oracle dst was created blank; read back the resolved region texel (1,1).
    let mut sw_full = vec![0u8; (w * h * 4) as usize];
    sw.read_texture(TextureId(dst), &mut sw_full).unwrap();
    let sw_avg = texel(&sw_full, w, 1, 1);
    assert!(near(sw_avg, AVG), "software oracle resolve = {sw_avg:?}, want {AVG:?}");
    assert!(
        near(texel(&out, w, 1, 1), sw_avg),
        "wgpu resolved pixel {:?} != software oracle {:?}",
        texel(&out, w, 1, 1), sw_avg
    );
}
