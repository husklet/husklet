use super::*;

/// A linked program's shader modules + render pipeline are created ONCE and re-referenced by their stable
/// IR ids on every later frame/draw that reuses the program (the program-keyed residency cache), so a reused
/// GskGpu program costs ZERO host shader compiles + pipeline builds after the first frame — the fix for the
/// per-draw shader recompile that stalled GTK4. A relink invalidates the cache and re-creates both.

#[test]
fn reused_program_is_not_recreated_across_frames() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // ---- shared resources + program, set up once ----
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    let verts: Vec<u8> = (0..48).map(|i| i as u8).collect();
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, FS);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);
    record::uniform_sampler(&mut c, 0, 0);

    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 2, 2, &[0xABu8; 16]);
    record::tex_parameter(&mut c, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    record::tex_parameter(&mut c, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    record::viewport(&mut c, [0, 0, 640, 480]);

    // ---- frame 1: first sight → the program's 2 shader modules + its pipeline are created once ----
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(
        count_shaders(&sink.batches[0]),
        2,
        "frame 1 compiles the program's 2 shader modules"
    );
    assert_eq!(
        count_pipelines(&sink.batches[0]),
        1,
        "frame 1 creates the program's pipeline"
    );

    // ---- frame 2: same program, same state → resident shaders + pipeline re-referenced, NOT re-emitted ----
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let f2 = &sink.batches[1];
    assert_eq!(
        count_shaders(f2),
        0,
        "a reused program emits NO new CreateShader on the 2nd frame"
    );
    assert_eq!(
        count_pipelines(f2),
        0,
        "a reused program emits NO new CreateRenderPipeline on the 2nd frame"
    );
    // The frame still draws: the resident pipeline id is bound in the pass.
    assert!(
        submit_ops(f2)
            .iter()
            .any(|e| matches!(e, Enc::SetPipeline(_))),
        "the reused pipeline is still bound"
    );

    // ---- relink: a new link generation invalidates the cache → shaders + pipeline created afresh ----
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(
        count_shaders(&sink.batches[2]),
        2,
        "a relinked program re-creates its shader modules"
    );
    assert_eq!(
        count_pipelines(&sink.batches[2]),
        1,
        "a relinked program re-creates its pipeline"
    );
}

/// Replaying the SAME warm program across MANY frames creates its IR shader modules + pipeline exactly
/// ONCE — the program-keyed residency cache holds the ids across frames, so steady-state frames (Chrome's
/// Skia re-uses each linked program on every `glFlush`) emit ZERO new CreateShader / CreateRenderPipeline and
/// the host residency stops growing. Then `glDeleteProgram` RETIRES the cached ids: the next submit destroys
/// the program's two shader modules + its pipeline, so the deleted program's residency is freed (and a
/// recycled GL name can't collide with its stale cache entry). This is the SHIM half of the Chrome residency
/// fix — without the delete-retire the cache would leak the modules/pipelines of every program Chrome tears
/// down over its lifetime.
#[test]
fn program_shaders_and_pipeline_created_once_across_n_frames_then_retired_on_delete() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // ---- shared resources + program, set up once (same shape as the reuse test above) ----
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    let verts: Vec<u8> = (0..48).map(|i| i as u8).collect();
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, FS);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);
    record::uniform_sampler(&mut c, 0, 0);

    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 2, 2, &[0xABu8; 16]);
    record::tex_parameter(&mut c, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    record::tex_parameter(&mut c, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    record::viewport(&mut c, [0, 0, 640, 480]);

    // ---- replay N steady-state frames of the same warm program ----
    const N: usize = 5;
    for _ in 0..N {
        record::use_program(&mut c, prog);
        record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
        assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    }
    // Across ALL N frames the program's 2 shader modules + 1 pipeline were created EXACTLY once — not per frame.
    let total_shaders: usize = sink.batches.iter().map(|b| count_shaders(b)).sum();
    let total_pipes: usize = sink.batches.iter().map(|b| count_pipelines(b)).sum();
    assert_eq!(
        total_shaders, 2,
        "{N} frames create the program's 2 shader modules once, not 2*N"
    );
    assert_eq!(
        total_pipes, 1,
        "{N} frames create the program's pipeline once, not N"
    );

    // Capture the resident ids minted in frame 1 (vs then fs, then the pipeline) so the delete can be asserted
    // to destroy EXACTLY them.
    let (vs_ir, fs_ir) = {
        let mut ids = sink.batches[0].iter().filter_map(|c| match c {
            Cmd::CreateShader { id, .. } => Some(*id),
            _ => None,
        });
        (
            ids.next().expect("vertex module id"),
            ids.next().expect("fragment module id"),
        )
    };
    let pipe_ir = sink.batches[0]
        .iter()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(id, _) => Some(*id),
            _ => None,
        })
        .expect("resident pipeline id");

    // ---- glDeleteProgram retires the cached shader modules + pipeline ----
    record::delete_program(&mut c, prog);
    assert!(
        c.has_pending_destroys(),
        "glDeleteProgram queues the program's persistent destroys"
    );

    // A swap with nothing to draw returns false but still flushes the queued destroys.
    assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
    let destroy_batch = sink.batches.last().unwrap();
    assert!(
        destroy_batch.contains(&Cmd::DestroyShader(vs_ir)),
        "deleted program's vertex module is destroyed: {destroy_batch:?}"
    );
    assert!(
        destroy_batch.contains(&Cmd::DestroyShader(fs_ir)),
        "deleted program's fragment module is destroyed: {destroy_batch:?}"
    );
    assert!(
        destroy_batch.contains(&Cmd::DestroyPipeline(pipe_ir)),
        "deleted program's pipeline is destroyed: {destroy_batch:?}"
    );
    assert!(
        !c.has_pending_destroys(),
        "destroys cleared after a successful submit"
    );
}

/// Two draws of the SAME program in ONE frame (the GskGpu shape: one program batched across many draws)
/// share a single set of shader modules + pipeline — the 2nd draw adds no CreateShader / CreateRenderPipeline.
#[test]
fn same_program_across_draws_in_one_frame_compiles_once() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    // A second draw with the identical bound state (same program, same layout) within the same frame.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    assert_eq!(
        count_shaders(batch),
        2,
        "two draws of one program compile its 2 shader modules once, not per draw"
    );
    assert_eq!(
        count_pipelines(batch),
        1,
        "two draws of one program build its pipeline once, not per draw"
    );
    // Both draws lowered into the single pass (two Draw ops share the one resident pipeline).
    assert_eq!(
        submit_ops(batch)
            .iter()
            .filter(|e| matches!(e, Enc::Draw { .. }))
            .count(),
        2,
        "both draws are present in the pass"
    );
}
