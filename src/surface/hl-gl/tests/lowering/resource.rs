use super::*;

#[test]
fn textured_quad_uploads_buffer_and_texture() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // a vertex buffer upload: CreateBuffer(VERTEX) immediately followed by its WriteBuffer.
    let vbo_pos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::VERTEX))
        .expect("vertex CreateBuffer");
    let vbo_id = match &batch[vbo_pos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    assert!(
        matches!(&batch[vbo_pos + 1], Cmd::WriteBuffer { id, offset: 0, data } if *id == vbo_id && data.len() == 48)
    );

    // a texture upload: CreateTexture + CreateSampler + a COPY_SRC staging buffer + WriteBuffer.
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateSampler(_, _))));
    let stage_pos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::COPY_SRC))
        .expect("staging CreateBuffer");
    let stage_id = match &batch[stage_pos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    assert!(
        matches!(&batch[stage_pos + 1], Cmd::WriteBuffer { id, offset: 0, data } if *id == stage_id && data.len() == 16)
    );

    // the shader carries the forwarded GLSL as `Glsl` payloads (one per stage); a render pipeline uses them.
    assert!(batch.iter().any(|c| matches!(
        c,
        Cmd::CreateShader {
            kind: ShaderPayloadKind::Glsl,
            ..
        }
    )));
    assert_eq!(
        batch
            .iter()
            .filter(|c| matches!(
                c,
                Cmd::CreateShader {
                    kind: ShaderPayloadKind::Glsl,
                    ..
                }
            ))
            .count(),
        2,
        "vertex + fragment GLSL are two separate Glsl shader modules"
    );
    assert!(batch
        .iter()
        .any(|c| matches!(c, Cmd::CreateRenderPipeline(_, _))));
}

#[test]
fn equivalent_sampler_descriptors_are_resident_across_frames() {
    let mut context = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut context);
    for _ in 1..189 {
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 6);
    }
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());

    let first = &sink.batches[0];
    let samplers = first
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateSampler(id, _) => Some(*id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        samplers.len(),
        1,
        "189 descriptor-equivalent draws must create one sampler"
    );
    assert!(!first.contains(&Cmd::DestroySampler(samplers[0])));

    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert!(
        !sink.batches[1]
            .iter()
            .any(|command| matches!(command, Cmd::CreateSampler(..) | Cmd::DestroySampler(..))),
        "an unchanged descriptor must reuse its resident sampler"
    );
}

#[test]
fn sampler_parameter_mutation_selects_a_distinct_immutable_sampler() {
    let mut context = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut context);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let first = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateSampler(id, _) => Some(*id),
            _ => None,
        })
        .unwrap();

    record::tex_parameter(&mut context, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    record::tex_parameter(&mut context, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let second = sink.batches[1]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateSampler(id, _) => Some(*id),
            _ => None,
        })
        .expect("the changed complete descriptor creates one immutable sampler");
    assert_ne!(first, second);
    assert!(!sink.batches[1].contains(&Cmd::DestroySampler(first)));

    context.retire_all();
    let destroyed = context
        .pending_destroys()
        .iter()
        .filter_map(|command| match command {
            Cmd::DestroySampler(id) => Some(*id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(destroyed.iter().filter(|id| **id == first).count(), 1);
    assert_eq!(destroyed.iter().filter(|id| **id == second).count(), 1);
}

#[test]
fn bound_sampler_object_overrides_texture_state_and_mutations_change_residency() {
    let mut context = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut context);

    let sampler = context.samplers.gen();
    hl_gl::service::es3::bind_sampler(&mut context, 0, sampler);
    for (parameter, integer, float) in [
        (
            GL_TEXTURE_MIN_FILTER,
            GL_LINEAR_MIPMAP_LINEAR as i32,
            GL_LINEAR_MIPMAP_LINEAR as f32,
        ),
        (
            GL_TEXTURE_WRAP_R,
            GL_MIRRORED_REPEAT as i32,
            GL_MIRRORED_REPEAT as f32,
        ),
        (GL_TEXTURE_MIN_LOD, 2, 2.0),
        (GL_TEXTURE_MAX_LOD, 7, 7.0),
    ] {
        hl_gl::service::es3::sampler_parameter(&mut context, sampler, parameter, integer, float);
    }
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let (first, descriptor) = sink.batches[0]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateSampler(id, descriptor) => Some((*id, descriptor)),
            _ => None,
        })
        .last()
        .expect("bound sampler object lowers");
    assert_eq!(
        descriptor.mip_filter,
        hl_gpu::protocol::model::enums::Filter::Linear
    );
    assert_eq!(
        descriptor.address_w,
        hl_gpu::protocol::model::enums::AddressMode::MirrorRepeat
    );
    assert_eq!(descriptor.lod_min_clamp, 2.0);
    assert_eq!(descriptor.lod_max_clamp, 7.0);
    assert_eq!(descriptor.compare, None);

    hl_gl::service::es3::sampler_parameter(
        &mut context,
        sampler,
        GL_TEXTURE_WRAP_R,
        GL_CLAMP_TO_EDGE as i32,
        GL_CLAMP_TO_EDGE as f32,
    );
    hl_gl::service::es3::sampler_parameter(&mut context, sampler, GL_TEXTURE_MIN_LOD, 3, 3.0);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let (second, descriptor) = sink.batches[1]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateSampler(id, descriptor) => Some((*id, descriptor)),
            _ => None,
        })
        .expect("sampler-object mutation selects a distinct descriptor");
    assert_ne!(second, first);
    assert_eq!(
        descriptor.address_w,
        hl_gpu::protocol::model::enums::AddressMode::ClampToEdge
    );
    assert_eq!(descriptor.lod_min_clamp, 3.0);
}

#[test]
fn comparison_sampler_state_is_preserved_in_the_residency_key() {
    let mut context = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut context);
    let sampler = context.samplers.gen();
    hl_gl::service::es3::bind_sampler(&mut context, 0, sampler);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let ordinary = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateSampler(id, _) => Some(*id),
            _ => None,
        })
        .unwrap();

    hl_gl::service::es3::sampler_parameter(
        &mut context,
        sampler,
        GL_TEXTURE_COMPARE_MODE,
        GL_COMPARE_REF_TO_TEXTURE as i32,
        GL_COMPARE_REF_TO_TEXTURE as f32,
    );
    hl_gl::service::es3::sampler_parameter(
        &mut context,
        sampler,
        GL_TEXTURE_COMPARE_FUNC,
        GL_GEQUAL as i32,
        GL_GEQUAL as f32,
    );
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let (comparison, descriptor) = sink.batches[1]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateSampler(id, descriptor) => Some((*id, descriptor)),
            _ => None,
        })
        .expect("comparison state selects a new sampler");
    assert_ne!(comparison, ordinary);
    assert_eq!(
        descriptor.compare,
        Some(hl_gpu::protocol::model::enums::compare::GREATER_EQUAL)
    );
}

/// `glDeleteTextures`/`glDeleteBuffers` of a resource whose resident IR was created in a prior frame RETIRE
/// that residency: the next submitted frame carries a matching `DestroyTexture`/`DestroyBuffer` for the exact
/// ids, so a long-running multi-frame app (Chrome) does not climb the host's per-connection residency ledger
/// to its cap. Bounded-residency is the fix for the Chrome swap-frame `ResourceLimit("connection residency")`
/// NACK. A single-frame app that deletes mid-frame still renders (the destroy rides the frame's tail, after
/// its Submits) — proven by the first frame lowering normally below.
#[test]
fn deleted_texture_and_buffer_retire_their_resident_ir() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // Frame 1: a textured quad — creates the resident sampled-texture + vertex-buffer IR ids.
    record_textured_quad(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch0 = sink.batches[0].clone();
    // The resident sampled texture is the SAMPLED|COPY_DST CreateTexture (not a render target / placeholder;
    // here the 2x2 texture with real pixels is the only sampled upload).
    let tex_ir = batch0
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d)
                if d.usage
                    == (hl_gpu::protocol::model::enums::texture_usage::SAMPLED
                        | hl_gpu::protocol::model::enums::texture_usage::COPY_DST)
                    && d.width == 2
                    && d.height == 2 =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("resident sampled texture");
    let vbo_ir = batch0
        .iter()
        .find_map(|c| match c {
            Cmd::CreateBuffer(id, d) if d.usage == buffer_usage::VERTEX => Some(*id),
            _ => None,
        })
        .expect("resident vertex buffer");

    // Frame 1 must NOT destroy these persistent resources (they are re-referenced next frame).
    assert!(
        !batch0.contains(&Cmd::DestroyTexture(tex_ir)),
        "frame-1 must not retire a live texture"
    );
    assert!(
        !batch0.contains(&Cmd::DestroyBuffer(vbo_ir)),
        "frame-1 must not retire a live buffer"
    );

    // Now delete both GL objects. The next swap (no draws) submits a standalone destroy batch.
    // The GL names: the textured-quad helper mints vbo=1, tex=1 (first of each kind).
    c.delete_texture(1);
    c.delete_buffer(1);
    assert!(
        c.has_pending_destroys(),
        "delete must queue persistent destroys"
    );

    // A swap with nothing to draw returns false but still flushes the queued destroys.
    assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
    let destroy_batch = sink.batches.last().unwrap();
    assert!(
        destroy_batch.contains(&Cmd::DestroyTexture(tex_ir)),
        "deleted texture's IR is destroyed: {destroy_batch:?}"
    );
    assert!(
        destroy_batch.contains(&Cmd::DestroyBuffer(vbo_ir)),
        "deleted buffer's IR is destroyed: {destroy_batch:?}"
    );
    assert!(
        !c.has_pending_destroys(),
        "destroys cleared after a successful submit"
    );
}
