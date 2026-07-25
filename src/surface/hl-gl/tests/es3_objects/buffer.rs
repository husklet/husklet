use super::*;

#[test]
fn get_buffer_parameteriv_reports_size_and_usage() {
    let mut c = ctx();
    let b = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, b);
    record::buffer_data(
        &mut c,
        GL_ARRAY_BUFFER,
        &[0u8; 48],
        0x88E4, /* GL_STATIC_DRAW */
    );

    assert_eq!(
        query::get_buffer_parameteriv(&c, GL_ARRAY_BUFFER, GL_BUFFER_SIZE),
        48
    );
    assert_eq!(
        query::get_buffer_parameteriv(&c, GL_ARRAY_BUFFER, GL_BUFFER_USAGE),
        0x88E4
    );
    // An unknown pname reads 0.
    assert_eq!(
        query::get_buffer_parameteriv(&c, GL_ARRAY_BUFFER, 0xBEEF),
        0
    );
}

#[test]
fn copy_buffer_sub_data_copies_bytes_between_buffers() {
    let mut c = ctx();
    let src = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, src);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[1, 2, 3, 4, 5, 6, 7, 8], 0);

    let dst = c.buffers.gen();
    record::bind_buffer(&mut c, GL_COPY_WRITE_BUFFER, dst);
    record::buffer_data(&mut c, GL_COPY_WRITE_BUFFER, &[0u8; 8], 0);

    record::copy_buffer_sub_data(&mut c, GL_ARRAY_BUFFER, GL_COPY_WRITE_BUFFER, 2, 0, 4);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(&c.buffers.get(dst).unwrap().data[0..4], &[3, 4, 5, 6]);
}

// ---- deletion lifecycle: glDeleteQueries / glDeleteTransformFeedbacks / glDeleteProgramPipelines ---

#[test]
fn get_integer_indexed_reads_back_indexed_buffer_bindings() {
    let mut c = ctx();
    let ubo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_UNIFORM_BUFFER, ubo);
    record::buffer_data(&mut c, GL_UNIFORM_BUFFER, &[0u8; 64], 0x88E4);
    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, 2, ubo);

    assert_eq!(
        query::get_integer_indexed(&c, GL_UNIFORM_BUFFER_BINDING, 2),
        ubo as i64,
        "glGetIntegeri_v(GL_UNIFORM_BUFFER_BINDING, 2) reports the buffer bound at index 2"
    );
    // An unbound index reads 0.
    assert_eq!(
        query::get_integer_indexed(&c, GL_UNIFORM_BUFFER_BINDING, 5),
        0
    );
}
