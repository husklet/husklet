use super::harness::*;

// 5. RENDER-TO-LAYER / RENDER-TO-MIP — a DOCUMENTED whole-texture limit of the VK→IR lowering.
// ===================================================================================================

/// Rendering into a specific array layer / mip of a texture is NOT expressible through this lowering:
///   * `vkCreateImage` models single-mip (`mip_levels == 1`) single-layer (`depth == 1`) 2D images, so
///     there is no layer/mip to select as a render target in the first place; and
///   * the IR `ColorAttachment`/`DepthAttachment` carry only a whole `texture` id — there is NO mip/layer
///     subresource selector on a render attachment (that lives in the protocol crate, which a concurrent
///     agent owns and this task must not touch).
/// So a render-to-layer/mip request collapses to rendering the WHOLE texture. This test pins that truth:
/// the created texture is single-mip/single-layer, and the attachment names the whole texture id.
#[test]
fn render_to_layer_and_mip_is_a_documented_whole_texture_limit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    let tex = sampled_color(&mut d, &mut sink, 512, 512);
    let tex_ir = img_ir(&d, tex);

    // The created texture is single-mip, single-layer (depth 1) — no layer/mip exists to target.
    match sink
        .batches
        .iter()
        .flatten()
        .find(|c| matches!(c, Cmd::CreateTexture(id, _) if *id == tex_ir))
    {
        Some(Cmd::CreateTexture(_, desc)) => {
            assert_eq!(
                desc.mip_levels, 1,
                "vkCreateImage models a single-mip image (no mip to render into)"
            );
            assert_eq!(
                desc.depth, 1,
                "vkCreateImage models a single-layer 2D image (no array layer to render into)"
            );
        }
        other => panic!("expected CreateTexture, got {other:?}"),
    }

    // Rendering into it names the WHOLE texture — the ColorAttachment has no mip/layer field to select one.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_begin_rendering(&mut d, cb, &[color_clear(tex, [0.0; 4])], None).unwrap();
    d.end_render_pass(cb).unwrap();
    d.end_command_buffer(cb).unwrap();
    let enc = submit_encoder(&mut d, &mut sink, cb);

    // The attachment is exactly `ColorAttachment { texture, load, clear, store }` — no subresource. The
    // whole-struct equality below is the proof: were a layer/mip selector present it would appear here.
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: tex_ir,
                    load: LoadOp::Clear,
                    clear: [0.0; 4],
                    store: true
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ],
        "a render attachment names the whole texture (no layer/mip subresource in the IR)"
    );
}
