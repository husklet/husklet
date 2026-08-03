use super::*;

#[test]
fn program_pipeline_stage_binding() {
    let mut c = ctx();
    // A separable program to bind into a stage.
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        vs,
        "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n",
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
    assert!(record::link_program(&mut c, prog));

    let pipe = c.gen_program_pipeline();
    assert_ne!(pipe, 0);
    c.bind_program_pipeline(pipe);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(c.is_program_pipeline(pipe));

    es3::use_program_stages(
        &mut c,
        pipe,
        GL_VERTEX_SHADER_BIT | GL_FRAGMENT_SHADER_BIT,
        prog,
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        es3::get_program_pipelineiv(&mut c, pipe, GL_VERTEX_SHADER),
        Some(prog as i32)
    );
    assert_eq!(
        es3::get_program_pipelineiv(&mut c, pipe, GL_FRAGMENT_SHADER),
        Some(prog as i32)
    );

    es3::active_shader_program(&mut c, pipe, prog);
    assert_eq!(
        es3::get_program_pipelineiv(&mut c, pipe, GL_ACTIVE_PROGRAM),
        Some(prog as i32)
    );

    // An unknown pipeline is GL_INVALID_OPERATION.
    assert_eq!(
        es3::get_program_pipelineiv(&mut c, 9999, GL_ACTIVE_PROGRAM),
        None
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}
#[test]
fn delete_program_pipeline_object_makes_it_no_longer_a_pipeline() {
    let mut c = ctx();
    let pipe = c.gen_program_pipeline();
    c.bind_program_pipeline(pipe);
    assert!(c.is_program_pipeline(pipe));

    c.delete_program_pipeline(pipe);
    assert!(
        !c.is_program_pipeline(pipe),
        "glDeleteProgramPipelines drops the object"
    );
    // A getter on the deleted pipeline is GL_INVALID_OPERATION.
    assert_eq!(
        es3::get_program_pipelineiv(&mut c, pipe, GL_ACTIVE_PROGRAM),
        None
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

/// A refused compile must report `GL_FALSE` and say why. `GL_INFO_LOG_LENGTH` was hard-coded to zero, so
/// even once a compile could fail there was nowhere for the reason to go.
#[test]
fn a_refused_compile_reports_false_and_a_diagnostic() {
    let mut c = ctx();
    let sh = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(
        &mut c,
        sh,
        "#version 300 es\nprecision highp float;\nout vec4 o;\n\
         void main() { o = vec4(float(bitCount(7))); }\n",
    );
    record::compile_shader(&mut c, sh);

    assert_eq!(
        query::get_shaderiv(&c, sh, GL_COMPILE_STATUS),
        GL_FALSE as i32,
        "a 3.10 built-in under #version 300 es must not compile"
    );
    let log = query::shader_info_log(&c, sh);
    assert!(
        log.contains("bitCount"),
        "the log names the construct: {log:?}"
    );
    assert!(
        log.contains("3.10"),
        "and the version that introduced it: {log:?}"
    );
    assert_eq!(
        query::get_shaderiv(&c, sh, GL_INFO_LOG_LENGTH),
        log.len() as i32 + 1,
        "GL_INFO_LOG_LENGTH must match the log, not report zero"
    );

    // An ordinary 3.00 shader still compiles with an empty log.
    let ok = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(
        &mut c,
        ok,
        "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main() { o = vec4(1.0); }\n",
    );
    record::compile_shader(&mut c, ok);
    assert_eq!(
        query::get_shaderiv(&c, ok, GL_COMPILE_STATUS),
        GL_TRUE as i32
    );
    assert_eq!(query::get_shaderiv(&c, ok, GL_INFO_LOG_LENGTH), 0);
}
