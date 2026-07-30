use super::harness::*;
use hl_gpu::protocol::model::descriptor::TextureViewDesc;
use hl_gpu::protocol::model::enums::{TextureAspect, TextureDim};
use hl_gpu::CommandSink;

#[test]
fn render_to_cube_layer_and_mip_uses_a_typed_texture_view() {
    let mut device = dev();
    let mut sink = RecordingSink::with_full_caps();
    let image = create::create_image_layers(
        &mut device,
        &mut sink,
        8,
        8,
        12,
        4,
        true,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let source = device.images.get(&image).unwrap().clone();
    let view = device.alloc_handle();
    let view_ir = device.alloc_ir();
    sink.submit(&[Cmd::CreateTextureView(
        view_ir,
        TextureViewDesc {
            texture: source.ir_id,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            aspect: TextureAspect::All,
            base_mip: 2,
            mip_count: 1,
            base_layer: 7,
            layer_count: 1,
        },
    )])
    .unwrap();
    device.images.insert(
        view,
        hl_vulkan::model::memory::ImageRec {
            ir_id: view_ir,
            width: 2,
            height: 2,
            depth: 1,
            dim: TextureDim::D2,
            layers: 1,
            mip_levels: 1,
            ..source
        },
    );

    let command_buffer = device.allocate_command_buffer();
    device.begin_command_buffer(command_buffer, false).unwrap();
    record::cmd_begin_rendering(
        &mut device,
        command_buffer,
        &[color_clear(view, [0.0; 4])],
        None,
    )
    .unwrap();
    device.end_render_pass(command_buffer).unwrap();
    device.end_command_buffer(command_buffer).unwrap();

    assert_eq!(
        submit_encoder(&mut device, &mut sink, command_buffer),
        vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: view_ir,
                    load: LoadOp::Clear,
                    clear: [0.0; 4],
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ]
    );
}
