//! wgpu executor coverage for the Phase-3 texture-to-texture copy + scaled blit IR ops on a real Metal
//! device. This is the second required executor for the slice (alongside the bespoke Metal backend): the
//! SAME IR the software oracle and Metal backend run flows through `WgpuBackend` and the destination reads
//! back matching pixels. Skips when no Metal adapter is present. macOS only.

#![cfg(target_os = "macos")]

use hl_gpu::backend::GpuBackend;
use hl_gpu::id::TextureId;
use hl_gpu::ir::*;
use hl_gpu_wgpu::WgpuBackend;

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

/// Upload `pattern` (tight RGBA rows) into a fresh `w`x`h` texture `id` via a staging buffer.
fn seed_cmds(id: u32, w: u32, h: u32, usage: u32, pattern: &[u8]) -> Vec<Cmd> {
    let stage = 900 + id;
    vec![
        Cmd::CreateTexture(id, tex_desc(w, h, usage | texture_usage::COPY_DST)),
        Cmd::CreateBuffer(stage, BufferDesc { size: pattern.len() as u64, usage: buffer_usage::COPY_SRC, label: String::new() }),
        Cmd::WriteBuffer { id: stage, offset: 0, data: pattern.to_vec() },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture { src: stage, src_offset: 0, bytes_per_row: w * 4, dst: id, mip: 0, width: w, height: h }],
            signal: None,
        }),
    ]
}

fn sub0() -> TextureSubresource {
    TextureSubresource::base()
}

fn near(got: [u8; 4], want: [u8; 4]) -> bool {
    got.iter().zip(want).all(|(g, w)| (*g as i16 - w as i16).abs() <= 3)
}

#[test]
fn copy_texture_to_texture_moves_the_requested_region() {
    let Ok(mut be) = WgpuBackend::new() else {
        eprintln!("skipping: no Metal adapter");
        return;
    };
    // src 4x4 where texel (x,y) = [x*10, y*10, 0, 255].
    let mut src = Vec::new();
    for y in 0..4u8 {
        for x in 0..4u8 {
            src.extend_from_slice(&[x * 10, y * 10, 0, 255]);
        }
    }
    let mut cmds = seed_cmds(1, 4, 4, texture_usage::SAMPLED | texture_usage::COPY_SRC, &src);
    cmds.push(Cmd::CreateTexture(2, tex_desc(4, 4, texture_usage::COPY_DST | texture_usage::COPY_SRC)));
    cmds.push(Cmd::Submit(CommandBuffer {
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
    }));
    hl_gpu::replay::replay_stream(&mut be, &encode_stream(&cmds)).expect("replay copy");

    let mut out = vec![0u8; 64];
    be.read_texture(TextureId(2), &mut out).unwrap();
    let px = |x: usize, y: usize| { let o = (y * 4 + x) * 4; [out[o], out[o + 1], out[o + 2], out[o + 3]] };
    assert!(near(px(0, 0), [10, 10, 0, 255]), "got {:?}", px(0, 0));
    assert!(near(px(1, 0), [20, 10, 0, 255]), "got {:?}", px(1, 0));
    assert!(near(px(1, 1), [20, 20, 0, 255]), "got {:?}", px(1, 1));
}

#[test]
fn blit_nearest_scales_up_by_block_replication() {
    let Ok(mut be) = WgpuBackend::new() else {
        eprintln!("skipping: no Metal adapter");
        return;
    };
    // 2x2 src: red, green / blue, white.
    let src: [u8; 16] = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    let mut cmds = seed_cmds(1, 2, 2, texture_usage::SAMPLED | texture_usage::COPY_SRC, &src);
    cmds.push(Cmd::CreateTexture(3, tex_desc(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)));
    cmds.push(Cmd::Submit(CommandBuffer {
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
    }));
    hl_gpu::replay::replay_stream(&mut be, &encode_stream(&cmds)).expect("replay blit");

    let mut out = vec![0u8; 64];
    be.read_texture(TextureId(3), &mut out).unwrap();
    let px = |x: usize, y: usize| { let o = (y * 4 + x) * 4; [out[o], out[o + 1], out[o + 2], out[o + 3]] };
    assert!(near(px(0, 0), [255, 0, 0, 255]), "top-left got {:?}", px(0, 0));
    assert!(near(px(1, 1), [255, 0, 0, 255]), "top-left block got {:?}", px(1, 1));
    assert!(near(px(2, 0), [0, 255, 0, 255]), "top-right got {:?}", px(2, 0));
    assert!(near(px(0, 2), [0, 0, 255, 255]), "bottom-left got {:?}", px(0, 2));
    assert!(near(px(3, 3), [255, 255, 255, 255]), "bottom-right got {:?}", px(3, 3));
}
