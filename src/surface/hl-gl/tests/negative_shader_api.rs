use hl_gl::model::context::GlContext;
use hl_gl::model::glconst::{
    GL_FRAGMENT_SHADER, GL_INVALID_OPERATION, GL_NO_ERROR, GL_VERTEX_SHADER,
};
use hl_gl::service::record;

#[test]
fn shader_and_program_objects_share_one_name_namespace() {
    let mut context = GlContext::new();
    let vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    let fragment = record::create_shader(&mut context, GL_FRAGMENT_SHADER);
    let program = record::create_program(&mut context);
    assert_eq!((vertex, fragment, program), (1, 2, 3));
}

#[test]
fn attach_rejects_wrong_object_kinds_and_duplicate_stages() {
    let mut context = GlContext::new();
    let vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    let another_vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    let program = record::create_program(&mut context);

    record::attach_shader(&mut context, vertex, vertex);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::attach_shader(&mut context, program, program);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);

    record::attach_shader(&mut context, program, vertex);
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);
    record::attach_shader(&mut context, program, vertex);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::attach_shader(&mut context, program, another_vertex);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
}
