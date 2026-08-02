//! A refused encoder operation does not erase earlier native work or prevent later independent work.

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{texture_usage, Filter, LoadOp, TextureDim, TextureFormat};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuError, GpuExecutor, Limits, Session,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn texture(dim: TextureDim, width: u32, height: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width,
        height,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

#[test]
fn refusal_preserves_encoded_work_and_continues_to_later_operations() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove native submission behavior");
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );

    let observation_usage = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC;
    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, texture(TextureDim::D2, 1, 1, observation_usage)),
            Cmd::CreateTexture(2, texture(TextureDim::D2, 1, 1, observation_usage)),
            Cmd::CreateTexture(3, texture(TextureDim::D1, 1, 1, texture_usage::SAMPLED)),
            Cmd::CreateTexture(
                4,
                texture(TextureDim::D1, 1, 1, texture_usage::RENDER_TARGET),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::ClearRect {
                        texture: 1,
                        x: 0,
                        y: 0,
                        w: 1,
                        h: 1,
                        color: [0.0, 0.0, 0.0, 1.0],
                        base_array_layer: 0,
                        layer_count: 1,
                        mip_level: 0,
                    },
                    Enc::ClearRect {
                        texture: 2,
                        x: 0,
                        y: 0,
                        w: 1,
                        h: 1,
                        color: [0.0, 0.0, 0.0, 1.0],
                        base_array_layer: 0,
                        layer_count: 1,
                        mip_level: 0,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect(
        "persistent resources and poison clears must be established before the rejected submit",
    );

    let result = hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        1,
        &[Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [1.0, 0.0, 0.0, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
                Enc::BlitTexture {
                    src: 3,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    dst: 4,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                },
                Enc::ClearRect {
                    texture: 2,
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                    color: [0.0, 1.0, 0.0, 1.0],
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 0,
                },
            ],
            signal: None,
        })],
    );

    assert_eq!(result, Err(GpuError::Unsupported("wgpu: 1D blit source")));
    assert_eq!(
        exec.read_texture(&session.resources, 1).unwrap(),
        [255, 0, 0, 255],
        "native render-pass work encoded before the refusal must still be submitted"
    );
    assert_eq!(
        exec.read_texture(&session.resources, 2).unwrap(),
        [0, 255, 0, 255],
        "an independent operation after the refusal must still execute"
    );
}

#[test]
fn top_level_refusal_preserves_submits_on_both_sides() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove native submission behavior");
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );

    let usage = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC;
    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, texture(TextureDim::D2, 1, 1, usage)),
            Cmd::CreateTexture(2, texture(TextureDim::D2, 1, 1, usage)),
        ],
    )
    .expect("observation textures must exist before the refused batch");

    let clear = |texture, color| {
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::ClearRect {
                texture,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                color,
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 0,
            }],
            signal: None,
        })
    };
    let result = hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        1,
        &[
            clear(1, [1.0, 0.0, 0.0, 1.0]),
            Cmd::DestroySampler(4),
            clear(2, [0.0, 1.0, 0.0, 1.0]),
        ],
    );

    assert_eq!(
        result,
        Err(GpuError::UnknownId {
            kind: "sampler",
            id: 4,
        }),
        "the first top-level refusal must remain visible to the caller"
    );
    assert_eq!(
        exec.read_texture(&session.resources, 1).unwrap(),
        [255, 0, 0, 255],
        "valid native work before the stale sampler must execute"
    );
    assert_eq!(
        exec.read_texture(&session.resources, 2).unwrap(),
        [0, 255, 0, 255],
        "valid native work after the stale sampler must execute"
    );
}
