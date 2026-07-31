use super::*;

// ===================================================================================================
// out-of-range indices / units / locations / counts → guarded no-op, never a panic
// ===================================================================================================

/// Out-of-range attribute index, texture unit, and uniform location are guarded no-ops (never index past a
/// fixed array); a valid index afterwards takes effect.
#[test]
fn out_of_range_indices_are_guarded_no_ops() {
    let mut c = ctx();
    // A vertex-attrib index far past MAX_VERTEX_ATTRIBS raises GL_INVALID_VALUE and changes no state
    // (ES 3.0 §2.8) — it must never index past the fixed array either.
    record::vertex_attrib_pointer(&mut c, 9999, 4, GL_FLOAT, false, 0, 0);
    record::vertex_attrib_divisor(&mut c, 9999, 1);
    record::enable_vertex_attrib(&mut c, 9999);
    record::disable_vertex_attrib(&mut c, 9999);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A texture unit far past the modeled bank leaves the active unit unchanged.
    c.active_texture(GL_TEXTURE0 + 9999);
    assert_eq!(
        c.active_texture_unit(),
        0,
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
    // A uniform LOCATION that names nothing in the current program is GL_INVALID_OPERATION (ES 3.0
    // §2.11.7) — it must be an error, not a silent write into nothing, and above all not a slice panic.
    // Only location -1 is the defined silent no-op, and it never reaches the recorder.
    record::uniform_at(&mut c, 99999, &[0u8; 64]);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    record::uniform_sampler(&mut c, 99999, 3);
    record::program_uniform_at(&mut c, p, -7, &[0u8; 16]);
    let _ = c.take_gl_error();

    // A valid attribute index does take effect.
    record::vertex_attrib_pointer(&mut c, 0, 4, GL_FLOAT, false, 0, 0);
    record::enable_vertex_attrib(&mut c, 0);
    assert!(c.attributes()[0].enabled);
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
    assert_eq!(c.draws().len(), 2);
    // A negative count is rejected, recording nothing.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, -5);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(c.draws().len(), 2);
}

#[test]
fn negative_array_first_is_invalid_and_records_nothing() {
    let mut c = ctx();

    record::draw_arrays(&mut c, GL_TRIANGLES, -1, 3);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(c.draws().is_empty());

    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, -1, 3, 2);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(c.draws().is_empty());
}

/// A NEGATIVE `glViewport`/`glScissor` extent is `GL_INVALID_VALUE` and leaves the box unchanged (ES 3.0
/// §2.12.1 / §4.1.2); a huge but non-negative one is legal and merely clamped at lowering. A valid
/// viewport afterwards is stored, and no input faults. This asserted `GL_NO_ERROR` for the negative case.
#[test]
fn extreme_viewport_and_scissor_dims_do_not_panic() {
    let mut c = ctx();
    record::viewport(&mut c, [-1, -1, i32::MAX, i32::MAX]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR, "a huge extent is legal");
    record::scissor(&mut c, [i32::MIN, i32::MIN, -4, -4]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE, "a negative extent is not");
    record::viewport(&mut c, [0, 0, 320, 240]);
    assert_eq!(c.viewport(), [0, 0, 320, 240]);
}

#[test]
fn a_client_array_draw_span_past_the_capture_bound_is_refused_not_read() {
    let mut c = ctx();
    // A client vertex array carries no length, so the driver reads the span the draw names out of guest
    // memory. A maximal first/count over a 48-byte array names ~4 billion vertices: the span overflowed and
    // the read walked off the array. Spec-undefined (Mesa faults identically), but a guest must not be able
    // to kill the driver, so the span is refused instead.
    let array = [0.0f32; 12];
    record::enable_vertex_attrib(&mut c, 0);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 0, array.as_ptr() as usize);

    record::draw_arrays(&mut c, GL_TRIANGLES, i32::MAX, i32::MAX);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    assert!(c.draws().is_empty(), "a refused draw records nothing");

    // A span within the bound still records, so this narrows only what could not be served.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws().len(), 1);
}

// ---------------------------------------------------------------------------------------------------
// A draw with no vertex source must be REFUSED, not sent to the GPU
// ---------------------------------------------------------------------------------------------------

/// ES 3.0 §2.8: with a NON-DEFAULT vertex array object bound, an enabled attribute array whose buffer
/// binding is zero is `GL_INVALID_OPERATION` — client arrays are legal only on the default VAO, so such a
/// draw has no vertex source at all.
///
/// This is the error neither implementation raised, and letting the draw through is what destroyed the
/// context: it reached the GPU transport, failed there, and a transport failure marks the whole share
/// group LOST. Every later GL call then returns `R::default()` without reaching the model, which is why
/// `glCheckFramebufferStatus` answered `0x0000` — a value it cannot otherwise return — while the error
/// queue stayed empty and neither a fresh buffer nor a plain clear could recover the context.
#[test]
fn a_draw_whose_buffer_binding_reverted_is_refused_on_a_user_vao() {
    let mut c = ctx();
    let vao = c.gen_vertex_array();
    c.bind_vertex_array(vao);
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);
    let _ = c.take_gl_error();

    // Deleting the buffer correctly reverts the binding to zero — that part was never in doubt.
    assert!(c.delete_buffer_later(vbo));
    assert_eq!(c.attributes()[0].buffer, 0, "deletion unbinds, as it should");

    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(
        c.take_gl_error(),
        GL_INVALID_OPERATION,
        "the draw has no vertex source and must be refused"
    );
    assert!(
        c.draws().is_empty(),
        "and must record nothing, so it never reaches the transport"
    );

    // The context stays usable: a valid buffer re-established on the same attribute draws again.
    let replacement = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, replacement);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws().len(), 1, "recovery is the part that matters");
}

/// The DEFAULT vertex array object is the one place client arrays are legal, so the same shape there must
/// still record — that is the GTK client-array path and it must not be caught by the rule above.
#[test]
fn the_same_shape_on_the_default_vao_still_records() {
    let mut c = ctx();
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);
    let _ = c.take_gl_error();
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(
        c.take_gl_error(),
        GL_NO_ERROR,
        "a client array on the default VAO is legal"
    );
}
