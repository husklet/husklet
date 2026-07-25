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

    let pipe = c.program_pipelines.gen();
    assert_ne!(pipe, 0);
    c.bind_program_pipeline(pipe);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(c.program_pipelines.contains(pipe));

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
    let pipe = c.program_pipelines.gen();
    c.bind_program_pipeline(pipe);
    assert!(c.program_pipelines.contains(pipe));

    c.program_pipelines.delete(pipe);
    assert!(
        !c.program_pipelines.contains(pipe),
        "glDeleteProgramPipelines drops the object"
    );
    // A getter on the deleted pipeline is GL_INVALID_OPERATION.
    assert_eq!(
        es3::get_program_pipelineiv(&mut c, pipe, GL_ACTIVE_PROGRAM),
        None
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}
