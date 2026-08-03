use super::*;

#[test]
fn transform_feedback_state_machine_and_varyings() {
    let mut c = ctx();
    let tf = c.gen_transform_feedback();
    assert_ne!(tf, 0);
    assert!(!c.is_transform_feedback(tf), "reserved, not yet an object");

    es3::bind_transform_feedback(&mut c, GL_TRANSFORM_FEEDBACK, tf);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(c.is_transform_feedback(tf));

    // begin → pause → resume → end, all valid transitions.
    c.begin_transform_feedback(GL_TRIANGLES);
    assert!(c.transform_feedback_state().active);
    c.pause_transform_feedback();
    assert!(c.transform_feedback_state().paused);
    c.resume_transform_feedback();
    assert!(!c.transform_feedback_state().paused);
    c.end_transform_feedback();
    assert!(!c.transform_feedback_state().active);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A double-begin is GL_INVALID_OPERATION; a bad primitive mode is GL_INVALID_ENUM.
    c.begin_transform_feedback(GL_TRIANGLES);
    c.begin_transform_feedback(GL_TRIANGLES);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    c.end_transform_feedback();
    c.begin_transform_feedback(0xBEEF);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

#[test]
fn transform_feedback_binding_query_tracks_the_bound_object() {
    let mut c = ctx();
    let tf = c.gen_transform_feedback();

    assert_eq!(
        query::get_integerv(&c, GL_TRANSFORM_FEEDBACK_BINDING, &mut [0; 4]),
        1
    );
    let mut value = [0; 4];
    query::get_integerv(&c, GL_TRANSFORM_FEEDBACK_BINDING, &mut value);
    assert_eq!(value[0], 0);

    es3::bind_transform_feedback(&mut c, GL_TRANSFORM_FEEDBACK, tf);
    query::get_integerv(&c, GL_TRANSFORM_FEEDBACK_BINDING, &mut value);
    assert_eq!(value[0], tf as i32);

    es3::bind_transform_feedback(&mut c, GL_TRANSFORM_FEEDBACK, 0);
    query::get_integerv(&c, GL_TRANSFORM_FEEDBACK_BINDING, &mut value);
    assert_eq!(value[0], 0);
}

#[test]
fn transform_feedback_varyings_round_trip() {
    let mut c = ctx();
    // A linked program is required for glTransformFeedbackVaryings.
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        vs,
        "attribute vec2 aPos; varying vec4 vColor; varying vec3 vNormal;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); vColor=vec4(1.0); vNormal=vec3(0.0); }\n",
    );
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(
        &mut c,
        fs,
        "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0); }\n",
    );
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    let names = vec!["vColor".to_string(), "vNormal".to_string()];
    es3::transform_feedback_varyings(&mut c, prog, names, GL_INTERLEAVED_ATTRIBS);
    assert!(record::link_program(&mut c, prog));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        es3::transform_feedback_varying(&c, prog, 0).as_deref(),
        Some("vColor")
    );
    assert_eq!(
        es3::transform_feedback_varying(&c, prog, 1).as_deref(),
        Some("vNormal")
    );
    assert_eq!(es3::transform_feedback_varying(&c, prog, 2), None);

    // An unknown program is GL_INVALID_VALUE; a bad buffer mode is GL_INVALID_ENUM.
    es3::transform_feedback_varyings(&mut c, 9999, vec!["x".to_string()], GL_INTERLEAVED_ATTRIBS);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    es3::transform_feedback_varyings(&mut c, prog, vec!["x".to_string()], 0xBEEF);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

#[test]
fn transform_feedback_program_reflection_reports_point_size() {
    let mut c = ctx();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        vs,
        "#version 300 es\nvoid main(){ gl_Position=vec4(0.0); gl_PointSize=1.0; }\n",
    );
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(
        &mut c,
        fs,
        "#version 300 es\nprecision mediump float; out vec4 c; void main(){ c=vec4(1.0); }\n",
    );
    record::compile_shader(&mut c, fs);
    let program = record::create_program(&mut c);
    record::attach_shader(&mut c, program, vs);
    record::attach_shader(&mut c, program, fs);
    es3::transform_feedback_varyings(
        &mut c,
        program,
        vec!["gl_PointSize".to_string()],
        GL_INTERLEAVED_ATTRIBS,
    );
    assert!(record::link_program(&mut c, program));

    assert_eq!(
        query::get_programiv(&c, program, GL_TRANSFORM_FEEDBACK_VARYINGS),
        1
    );
    assert_eq!(
        query::get_programiv(&c, program, GL_TRANSFORM_FEEDBACK_BUFFER_MODE),
        GL_INTERLEAVED_ATTRIBS as i32
    );
    assert_eq!(
        query::get_programiv(&c, program, GL_TRANSFORM_FEEDBACK_VARYING_MAX_LENGTH),
        "gl_PointSize".len() as i32 + 1
    );
    assert_eq!(
        es3::transform_feedback_varying_info(&c, program, 0),
        Some(("gl_PointSize".to_string(), 1, GL_FLOAT))
    );

    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

#[test]
fn active_feedback_keeps_discarded_draw_and_advances_checked_cursor_and_query() {
    let mut c = ctx();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        vs,
        "#version 300 es\nvoid main(){ gl_Position=vec4(0.0); gl_PointSize=float(gl_VertexID+1); }",
    );
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(
        &mut c,
        fs,
        "#version 300 es\nprecision mediump float; out vec4 c; void main(){ c=vec4(1.0); }",
    );
    record::compile_shader(&mut c, fs);
    let program = record::create_program(&mut c);
    record::attach_shader(&mut c, program, vs);
    record::attach_shader(&mut c, program, fs);
    es3::transform_feedback_varyings(
        &mut c,
        program,
        vec!["gl_PointSize".into()],
        GL_INTERLEAVED_ATTRIBS,
    );
    assert!(record::link_program(&mut c, program));
    record::use_program(&mut c, program);

    let buffer = c.buffers.gen();
    record::bind_buffer(&mut c, GL_TRANSFORM_FEEDBACK_BUFFER, buffer);
    record::buffer_data(&mut c, GL_TRANSFORM_FEEDBACK_BUFFER, &[0; 64], 0);
    record::bind_buffer_base(&mut c, GL_TRANSFORM_FEEDBACK_BUFFER, 0, buffer);
    let query = c.gen_query();
    es3::begin_query(&mut c, GL_TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN, query);
    c.begin_transform_feedback(GL_POINTS);
    c.enable(GL_RASTERIZER_DISCARD);
    record::draw_arrays(&mut c, GL_POINTS, 0, 3);
    c.end_transform_feedback();
    c.end_query(GL_TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN);

    assert_eq!(c.recording_counts(), (1, 0));
    assert_eq!(c.transform_feedback_state().cursors[0], 12);
    assert_eq!(
        es3::get_query_objectuiv(&mut c, query, GL_QUERY_RESULT),
        Some(3)
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

#[test]
fn selected_array_element_records_for_every_primitive_and_buffer_mode() {
    for buffer_mode in [GL_INTERLEAVED_ATTRIBS, GL_SEPARATE_ATTRIBS] {
        for (primitive_mode, vertex_count) in [(GL_POINTS, 3), (GL_LINES, 4), (GL_TRIANGLES, 6)] {
            let mut c = ctx();
            let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
            record::shader_source(
                &mut c,
                vs,
                "#version 300 es\nout vec3 v[2]; void main(){ gl_Position=vec4(0.0); v[1]=vec3(float(gl_VertexID)); }",
            );
            record::compile_shader(&mut c, vs);
            let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
            record::shader_source(
                &mut c,
                fs,
                "#version 300 es\nprecision mediump float; out vec4 c; void main(){ c=vec4(1.0); }",
            );
            record::compile_shader(&mut c, fs);
            let program = record::create_program(&mut c);
            record::attach_shader(&mut c, program, vs);
            record::attach_shader(&mut c, program, fs);
            es3::transform_feedback_varyings(&mut c, program, vec!["v[1]".into()], buffer_mode);
            assert!(record::link_program(&mut c, program));
            record::use_program(&mut c, program);

            let buffer = c.buffers.gen();
            record::bind_buffer(&mut c, GL_TRANSFORM_FEEDBACK_BUFFER, buffer);
            record::buffer_data(&mut c, GL_TRANSFORM_FEEDBACK_BUFFER, &[0; 128], 0);
            record::bind_buffer_base(&mut c, GL_TRANSFORM_FEEDBACK_BUFFER, 0, buffer);
            c.begin_transform_feedback(primitive_mode);
            c.enable(GL_RASTERIZER_DISCARD);
            record::draw_arrays(&mut c, primitive_mode, 0, vertex_count);
            c.end_transform_feedback();

            let capture = c.draws()[0].transform_feedback.as_ref().unwrap();
            assert_eq!(capture.layout.strides[0], 12);
            assert_eq!(capture.layout.varyings[0].size, 1);
            assert_eq!(capture.layout.scalars[0].expression, "v[1][0]");
            assert_eq!(capture.layout.scalars[2].expression, "v[1][2]");
            assert_eq!(capture.vertices, vertex_count as u32);
            assert_eq!(c.take_gl_error(), GL_NO_ERROR);
        }
    }
}

// ---- program pipeline objects --------------------------------------------------------------------
#[test]
fn delete_transform_feedback_object_makes_it_no_longer_a_tf() {
    let mut c = ctx();
    let tf = c.gen_transform_feedback();
    es3::bind_transform_feedback(&mut c, GL_TRANSFORM_FEEDBACK, tf);
    assert!(c.is_transform_feedback(tf));

    c.delete_transform_feedback(tf);
    assert!(
        !c.is_transform_feedback(tf),
        "glDeleteTransformFeedbacks drops the object"
    );
    c.delete_transform_feedback(0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}
