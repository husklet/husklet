//! Native depth-blit proof: exercises the wgpu depth view, shader, render pass, staging preservation,
//! and exact nearest mapping on Metal rather than relying on the CPU oracle.

use hl_gpu::protocol::model::descriptor::{BufferDesc, Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, Filter, TextureDim, TextureFormat};
use hl_gpu::{Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn depth_texture() -> TextureDesc {
    TextureDesc {
        width: 4,
        height: 1,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Depth32Float,
        usage: texture_usage::COPY_SRC | texture_usage::COPY_DST,
        label: String::new(),
    }
}

fn bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

#[test]
fn nearest_depth_blit_runs_natively_and_preserves_outside_rect() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter required");
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let source = bytes(&[0.1, 0.2, 0.3, 0.4]);
    let destination = bytes(&[0.9; 4]);
    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, depth_texture()),
            Cmd::CreateTexture(2, depth_texture()),
            Cmd::CreateBuffer(1, BufferDesc { size: 16, usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST, label: String::new() }),
            Cmd::CreateBuffer(2, BufferDesc { size: 16, usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: source },
            Cmd::WriteBuffer { id: 2, offset: 0, data: destination },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 16, dst: 1, mip: 0, width: 4, height: 1 },
                    Enc::CopyBufferToTexture { src: 2, src_offset: 0, bytes_per_row: 16, dst: 2, mip: 0, width: 4, height: 1 },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d { width: 4, height: 1, depth: 1 },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d { x: 1, y: 0, z: 0 },
                        dst_extent: Extent3d { width: 2, height: 1, depth: 1 },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("native D32 depth blit must execute");

    assert_eq!(
        exec.read_texture(&session.resources, 2).expect("depth readback"),
        bytes(&[0.9, 0.2, 0.4, 0.9]),
    );
}
