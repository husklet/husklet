use super::*;

// ---------------------------------------------------------------------------------------------------
// honest "unlinked program presents nothing"
// ---------------------------------------------------------------------------------------------------

#[test]
fn a_draw_with_an_unlinked_program_presents_nothing() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // A program with attached-but-unlinked shaders bound.
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, v, VS);
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, FS);
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    // NOTE: no link_program.
    record::use_program(&mut c, p);
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // The unlinked program has no shader IR -> the draw can't be lowered -> nothing is presented.
    assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(sink.batches.is_empty());
    // The frame state is still reset for the next frame.
    assert!(c.draws().is_empty());
}
