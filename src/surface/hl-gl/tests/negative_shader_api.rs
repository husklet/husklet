use hl_gl::model::context::GlContext;
use hl_gl::model::glconst::{
    GL_FRAGMENT_SHADER, GL_INVALID_ENUM, GL_INVALID_OPERATION, GL_INVALID_VALUE, GL_NO_ERROR,
    GL_VERTEX_SHADER,
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
fn lifecycle_calls_distinguish_unknown_names_from_wrong_object_kinds() {
    let mut context = GlContext::new();
    assert_eq!(record::create_shader(&mut context, u32::MAX), 0);
    assert_eq!(context.take_gl_error(), GL_INVALID_ENUM);

    let shader = record::create_shader(&mut context, GL_VERTEX_SHADER);
    let program = record::create_program(&mut context);

    record::shader_source(&mut context, 99, "");
    assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);
    record::shader_source(&mut context, program, "");
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::shader_source(&mut context, shader, "void main(){}");
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);

    assert!(!record::link_program(&mut context, 99));
    assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);
    assert!(!record::link_program(&mut context, shader));
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);

    record::use_program(&mut context, 99);
    assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);
    record::use_program(&mut context, shader);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::use_program(&mut context, 0);
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);

    record::bind_attrib(&mut context, 99, 0, "position");
    assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);
    record::bind_attrib(&mut context, shader, 0, "position");
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::bind_attrib(&mut context, program, 0, "position");
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);

    record::detach_shader(&mut context, shader, shader);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::detach_shader(&mut context, program, program);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::detach_shader(&mut context, 99, shader);
    assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);
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
