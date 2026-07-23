use super::*;

// ===================================================================================================
// out-of-range indices / units / locations / counts → guarded no-op, never a panic
// ===================================================================================================

/// Out-of-range attribute index, texture unit, and uniform location are guarded no-ops (never index past a
/// fixed array); a valid index afterwards takes effect.
#[test]
fn out_of_range_indices_are_guarded_no_ops() {
    let mut c = ctx();
    // A vertex-attrib index far past MAX_VERTEX_ATTRIBS is a no-op.
    record::vertex_attrib_pointer(&mut c, 9999, 4, GL_FLOAT, false, 0, 0);
    record::vertex_attrib_divisor(&mut c, 9999, 1);
    record::enable_vertex_attrib(&mut c, 9999);
    record::disable_vertex_attrib(&mut c, 9999);
    // A texture unit far past the modeled bank leaves the active unit unchanged.
    c.active_texture(GL_TEXTURE0 + 9999);
    assert_eq!(
        c.active_texture, 0,
        "an out-of-range unit does not move the active unit"
    );
    // A uniform write to a bogus location on a linked program is a no-op (not a slice panic).
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        v,
        "attribute vec4 p; void main(){ gl_Position = p; }\n",
    );
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, "void main(){ gl_FragColor = vec4(1.0); }\n");
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    record::link_program(&mut c, p);
    record::use_program(&mut c, p);
    record::uniform_at(&mut c, 99999, &[0u8; 64]);
    record::uniform_sampler(&mut c, 99999, 3);
    record::program_uniform_at(&mut c, p, -7, &[0u8; 16]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A valid attribute index does take effect.
    record::vertex_attrib_pointer(&mut c, 0, 4, GL_FLOAT, false, 0, 0);
    record::enable_vertex_attrib(&mut c, 0);
    assert!(c.attr[0].enabled);
}

/// A huge `glDrawArrays` / `glDrawElements` count (near `i32::MAX`) with only VBO-backed attributes must
/// not overflow or unbounded-allocate at record time (no client-array capture runs) — the draw is just
/// recorded. A negative count is `GL_INVALID_VALUE`.
#[test]
fn huge_draw_counts_do_not_overflow_or_alloc() {
    let mut c = ctx();
    // No enabled client-side attributes → no per-vertex capture; a huge count is recorded verbatim.
    record::draw_arrays(&mut c, GL_TRIANGLES, i32::MAX - 1, i32::MAX);
    record::draw_elements(&mut c, GL_TRIANGLES, i32::MAX, GL_UNSIGNED_SHORT, 0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws.len(), 2);
    // A negative count is rejected, recording nothing.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, -5);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(c.draws.len(), 2);
}

/// Negative / huge `glViewport` + `glScissor` dimensions are stored without panicking; a valid viewport
/// afterwards is stored too. (The frame builder clamps at lowering; the record op never faults.)
#[test]
fn extreme_viewport_and_scissor_dims_do_not_panic() {
    let mut c = ctx();
    record::viewport(&mut c, [-1, -1, i32::MAX, i32::MAX]);
    record::scissor(&mut c, [i32::MIN, i32::MIN, -4, -4]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    record::viewport(&mut c, [0, 0, 320, 240]);
    assert_eq!(c.viewport, [0, 0, 320, 240]);
}
