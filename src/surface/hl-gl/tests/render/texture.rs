use super::*;
use hl_gl::model::texture::SharedPixels;
use std::sync::Arc;

const TEXTURED_FS: &str = "precision mediump float;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vec2(0.5)); }\n";

pub(super) fn program(context: &mut GlContext) {
    let vertex = record::create_shader(context, GL_VERTEX_SHADER);
    record::shader_source(context, vertex, VS);
    record::compile_shader(context, vertex);
    let fragment = record::create_shader(context, GL_FRAGMENT_SHADER);
    record::shader_source(context, fragment, TEXTURED_FS);
    record::compile_shader(context, fragment);
    let program = record::create_program(context);
    record::attach_shader(context, program, vertex);
    record::attach_shader(context, program, fragment);
    assert!(record::link_program(context, program));
    record::use_program(context, program);
    record::uniform_sampler(context, 0, 0);
}

pub(super) fn shared_texture(
    context: &mut GlContext,
    pixels: Arc<Vec<u8>>,
) -> (u32, Arc<SharedPixels>) {
    let name = context.textures.gen();
    record::bind_texture(context, GL_TEXTURE_2D, name);
    record::tex_image_2d(context, 4, 4, pixels.as_slice());
    let storage = Arc::new(SharedPixels::new(pixels));
    assert!(context.textures.bind_shared(name, Arc::clone(&storage)));
    (name, storage)
}

fn pixel_uploads(commands: &[Cmd], pixels: &[u8]) -> usize {
    commands
        .iter()
        .filter(|command| matches!(command, Cmd::WriteBuffer { data, .. } if data == pixels))
        .count()
}

#[test]
fn advanced_retired_snapshot_uploads_pixels_once_per_frame() {
    let mut context = ctx_64();
    program(&mut context);
    tri_vbo(&mut context, 8);
    let initial = Arc::new(vec![7; 4 * 4 * 4]);
    let (texture, storage) = shared_texture(&mut context, initial);

    for _ in 0..32 {
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    }
    let advanced = Arc::new(vec![193; 4 * 4 * 4]);
    storage.store(Arc::clone(&advanced));
    assert!(context.delete_texture(texture));

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("deferred frame");
    assert_eq!(pixel_uploads(&frame.cmds, advanced.as_slice()), 1);
    assert_eq!(pixel_uploads(&frame.cmds, advanced.as_slice()), 1);
}

#[test]
fn distinct_gl_views_of_one_shared_revision_upload_pixels_once() {
    let mut context = ctx_64();
    program(&mut context);
    tri_vbo(&mut context, 8);
    let initial = Arc::new(vec![17; 4 * 4 * 4]);
    let storage = Arc::new(SharedPixels::new(initial));

    for _ in 0..32 {
        let name = context.textures.gen();
        record::bind_texture(&mut context, GL_TEXTURE_2D, name);
        record::tex_image_2d(&mut context, 4, 4, &[17; 4 * 4 * 4]);
        assert!(context.textures.bind_shared(name, Arc::clone(&storage)));
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
        assert!(context.delete_texture(name));
    }

    let advanced = Arc::new(vec![211; 4 * 4 * 4]);
    storage.store(Arc::clone(&advanced));
    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("deferred frame");

    assert_eq!(pixel_uploads(&frame.cmds, advanced.as_slice()), 1);
    assert_eq!(pixel_uploads(&frame.cmds, advanced.as_slice()), 1);
}

#[test]
fn live_gl_views_of_one_shared_revision_share_one_upload() {
    let mut context = ctx_64();
    program(&mut context);
    tri_vbo(&mut context, 8);
    let pixels = Arc::new(vec![29; 4 * 4 * 4]);
    let storage = Arc::new(SharedPixels::new(Arc::clone(&pixels)));

    for _ in 0..2 {
        let name = context.textures.gen();
        record::bind_texture(&mut context, GL_TEXTURE_2D, name);
        record::tex_image_2d(&mut context, 4, 4, pixels.as_slice());
        assert!(context.textures.bind_shared(name, Arc::clone(&storage)));
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    }

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("shared views");
    assert_eq!(pixel_uploads(&frame.cmds, pixels.as_slice()), 1);
}

#[test]
fn stale_gl_views_reuse_only_the_same_immutable_pixel_backing() {
    let mut context = ctx_64();
    program(&mut context);
    tri_vbo(&mut context, 8);

    let seed = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, seed);
    record::tex_image_2d(&mut context, 4, 4, &[73; 4 * 4 * 4]);
    let backing = Arc::clone(&context.textures.get(seed).expect("seed texture").data);
    assert!(context.delete_texture(seed));

    for _ in 0..32 {
        let name = context.textures.gen();
        record::bind_texture(&mut context, GL_TEXTURE_2D, name);
        record::tex_image_2d(&mut context, 4, 4, &[73; 4 * 4 * 4]);
        context.textures.get_mut(name).expect("texture").data = Arc::clone(&backing);
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
        assert!(context.delete_texture(name));
    }

    // Equal bytes in a distinct Arc are not identity-equivalent and must retain their own IR resource.
    let distinct = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, distinct);
    record::tex_image_2d(&mut context, 4, 4, &[73; 4 * 4 * 4]);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(distinct));

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("deferred frame");
    let retired = frame
        .cmds
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "gl-retired-snapshot" => {
                Some(*id)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retired.len(), 2);
    for texture in retired {
        assert_eq!(
            frame
                .cmds
                .iter()
                .filter(|command| matches!(command, Cmd::DestroyTexture(id) if *id == texture))
                .count(),
            1
        );
    }
}

#[test]
fn distinct_shared_storages_do_not_alias_snapshot_uploads() {
    let mut context = ctx_64();
    program(&mut context);
    tri_vbo(&mut context, 8);

    let (first, first_storage) = shared_texture(&mut context, Arc::new(vec![11; 4 * 4 * 4]));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    let first_advanced = Arc::new(vec![101; 4 * 4 * 4]);
    first_storage.store(Arc::clone(&first_advanced));
    assert!(context.delete_texture(first));

    let (second, second_storage) = shared_texture(&mut context, Arc::new(vec![13; 4 * 4 * 4]));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    let second_advanced = Arc::new(vec![202; 4 * 4 * 4]);
    second_storage.store(Arc::clone(&second_advanced));
    assert!(context.delete_texture(second));

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("deferred frame");
    assert_eq!(pixel_uploads(&frame.cmds, first_advanced.as_slice()), 1);
    assert_eq!(pixel_uploads(&frame.cmds, second_advanced.as_slice()), 1);
}

#[test]
fn shared_image_snapshot_reuses_only_an_unchanged_cpu_revision() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    program(&mut context);
    tri_vbo(&mut context, 8);
    let first_pixels = Arc::new(vec![31; 4 * 4 * 4]);
    let (first, storage) = shared_texture(&mut context, Arc::clone(&first_pixels));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(first));
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let first_batch = &sink.batches[0];
    assert_eq!(pixel_uploads(first_batch, first_pixels.as_slice()), 1);

    let second = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, second);
    record::tex_image_2d(&mut context, 4, 4, first_pixels.as_slice());
    assert!(context.textures.bind_shared(second, Arc::clone(&storage)));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(second));
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert_eq!(storage.version(), 1);
    assert_eq!(pixel_uploads(&sink.batches[1], first_pixels.as_slice()), 0);

    let next_pixels = Arc::new(vec![157; 4 * 4 * 4]);
    storage.store(Arc::clone(&next_pixels));
    let third = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, third);
    record::tex_image_2d(&mut context, 4, 4, first_pixels.as_slice());
    assert!(context.textures.bind_shared(third, Arc::clone(&storage)));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(third));
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let replacement = &sink.batches[2];
    assert_eq!(pixel_uploads(replacement, next_pixels.as_slice()), 1);
}

#[test]
fn released_shared_storages_retire_residency_on_the_next_frame() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    program(&mut context);
    tri_vbo(&mut context, 8);
    let mut previous = None;

    for value in 1..=16 {
        let pixels = Arc::new(vec![value; 4 * 4 * 4]);
        let (texture, storage) = shared_texture(&mut context, Arc::clone(&pixels));
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
        assert!(context.delete_texture(texture));
        drop(storage);
        assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
        let batch = sink.batches.last().expect("accepted frame");
        if let Some(previous) = previous {
            assert!(batch
                .iter()
                .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == previous)));
        }
        previous = batch.iter().find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor)
                if descriptor.width == 4
                    && descriptor.height == 4
                    && descriptor.usage
                        == hl_gpu::protocol::model::enums::texture_usage::SAMPLED
                            | hl_gpu::protocol::model::enums::texture_usage::COPY_DST =>
            {
                Some(*id)
            }
            _ => None,
        });
        assert!(previous.is_some());
    }
}

#[test]
fn imported_flush_cleanup_keeps_unique_storage_churn_bounded() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    program(&mut context);
    tri_vbo(&mut context, 8);

    for value in 1..=16 {
        let pixels = Arc::new(vec![value; 4 * 4 * 4]);
        let (texture, storage) = shared_texture(&mut context, pixels);
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
        assert!(context.delete_texture(texture));
        drop(storage);
        assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
        assert_eq!(
            context
                .flush_retirements(&mut sink)
                .expect("custom imported cleanup"),
            1
        );
        assert!(sink
            .batches
            .last()
            .unwrap()
            .iter()
            .all(|command| matches!(command, Cmd::DestroyTexture(_))));
    }
}

#[test]
fn gpu_render_write_invalidates_shared_upload_and_sampling_uses_rendered_target() {
    use hl_gpu::protocol::model::descriptor::BindResource;

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    program(&mut context);
    tri_vbo(&mut context, 8);
    let pixels = Arc::new(vec![41; 4 * 4 * 4]);
    let (retired, storage) = shared_texture(&mut context, Arc::clone(&pixels));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(retired));
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let uploaded = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor)
                if descriptor.width == 4
                    && descriptor.height == 4
                    && descriptor.usage
                        == hl_gpu::protocol::model::enums::texture_usage::SAMPLED
                            | hl_gpu::protocol::model::enums::texture_usage::COPY_DST =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("resident shared CPU snapshot");

    let rendered = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, rendered);
    record::tex_image_2d(&mut context, 4, 4, pixels.as_slice());
    assert!(context.textures.bind_shared(rendered, Arc::clone(&storage)));
    context.textures.get_mut(rendered).unwrap().ir_format =
        hl_gpu::protocol::model::enums::TextureFormat::R8Unorm;
    let sibling = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, sibling);
    record::tex_image_2d(&mut context, 4, 4, pixels.as_slice());
    assert!(context.textures.bind_shared(sibling, Arc::clone(&storage)));
    context.textures.get_mut(sibling).unwrap().ir_format =
        hl_gpu::protocol::model::enums::TextureFormat::R8Unorm;
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        rendered,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let rendered_ir = sink.batches[1]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "offscreen-fbo" => Some(*id),
            _ => None,
        })
        .expect("shared image render target");

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    program(&mut context);
    record::bind_texture(&mut context, GL_TEXTURE_2D, sibling);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[2];
    assert!(
        batch
            .iter()
            .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == uploaded)),
        "the accepted GPU write retires the stale CPU snapshot"
    );
    assert_eq!(pixel_uploads(batch, pixels.as_slice()), 0);
    assert!(batch
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateBindGroup(_, descriptor) => Some(&descriptor.entries),
            _ => None,
        })
        .flatten()
        .any(|entry| matches!(entry.resource, BindResource::Texture { id } if id == rendered_ir)));

    let newer = Arc::new(vec![97; 4 * 4 * 4]);
    storage.store(Arc::clone(&newer));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert_eq!(pixel_uploads(&sink.batches[3], newer.as_slice()), 1);

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    assert!(!sink.batches[4]
        .iter()
        .any(|command| matches!(command, Cmd::CreateTexture(id, _) if *id == rendered_ir)));

    assert!(context.delete_texture(rendered));
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    program(&mut context);
    record::bind_texture(&mut context, GL_TEXTURE_2D, sibling);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let after_delete = &sink.batches[5];
    assert!(!after_delete
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == rendered_ir)));
    assert!(after_delete
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateBindGroup(_, descriptor) => Some(&descriptor.entries),
            _ => None,
        })
        .flatten()
        .any(|entry| matches!(entry.resource, BindResource::Texture { id } if id == rendered_ir)));
}

#[test]
fn deleted_shared_target_survives_deferred_flush_for_a_live_sibling() {
    use hl_gpu::protocol::model::descriptor::BindResource;

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let pixels = Arc::new(vec![53; 4 * 4 * 4]);
    let (producer, storage) = shared_texture(&mut context, Arc::clone(&pixels));
    let sibling = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, sibling);
    record::tex_image_2d(&mut context, 4, 4, pixels.as_slice());
    assert!(context.textures.bind_shared(sibling, storage));

    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        producer,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(producer));
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let rendered = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "gl-retired-fbo" => Some(*id),
            _ => None,
        })
        .expect("deferred shared render target");
    assert!(!sink.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == rendered)));

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    program(&mut context);
    record::bind_texture(&mut context, GL_TEXTURE_2D, sibling);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert!(sink.batches[1]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateBindGroup(_, descriptor) => Some(&descriptor.entries),
            _ => None,
        })
        .flatten()
        .any(|entry| matches!(entry.resource, BindResource::Texture { id } if id == rendered)));
}
