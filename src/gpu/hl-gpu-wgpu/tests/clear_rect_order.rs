mod gpu_harness;

use gpu_harness::{new_session, tex2d};
use hl_gpu::protocol::model::descriptor::ColorAttachment;
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp};
use hl_gpu::{Cmd, CommandBuffer, Enc};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

#[test]
fn clear_rect_runs_after_earlier_encoded_render_work() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove queue ordering");
    let mut session = new_session(&mut executor);

    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(1, 1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::Submit(CommandBuffer {
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
                    Enc::ClearRect {
                        texture: 1,
                        x: 0,
                        y: 0,
                        w: 1,
                        h: 1,
                        color: [0.0, 0.0, 1.0, 1.0],
                        base_array_layer: 0,
                        layer_count: 1,
                        mip_level: 0,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("render then clear");

    assert_eq!(
        executor.read_texture(&session.resources, 1).expect("readback"),
        [0, 0, 255, 255],
        "the logically later ClearRect must not be enqueued before the earlier render pass"
    );
}
