//! `glLinkProgram` against the compile status of the shaders it is asked to link.
//!
//! ES 3.0 §7.3: a link succeeds only when every attached shader carries `GL_COMPILE_STATUS` true. The
//! link used to consult only the uniform layout, so a program built from a shader this driver had
//! REFUSED still reported `GL_LINK_STATUS` true with an empty info log. The failure then surfaced far
//! downstream — the frame carrying a draw with no translated shader is refused whole, which a user sees
//! as missing geometry with no error anywhere near its cause.

use super::*;

/// A fragment shader this driver refuses: a GLSL ES 3.10 built-in under `#version 300 es`.
const REFUSED_FS: &str = "#version 300 es\nprecision highp float;\nout vec4 o;\n\
                          void main() { o = vec4(float(bitCount(7))); }\n";
const GOOD_FS: &str = "#version 300 es\nprecision highp float;\nuniform vec4 uTint;\nout vec4 o;\n\
                       void main() { o = uTint; }\n";
const GOOD_VS: &str =
    "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n";

fn shader(c: &mut GlContext, kind: u32, source: &str) -> u32 {
    let name = record::create_shader(c, kind);
    record::shader_source(c, name, source);
    record::compile_shader(c, name);
    name
}

fn program(c: &mut GlContext, vs: u32, fs: u32) -> u32 {
    let program = record::create_program(c);
    record::attach_shader(c, program, vs);
    record::attach_shader(c, program, fs);
    program
}

#[test]
fn a_program_whose_shader_was_refused_does_not_link() {
    let mut c = ctx();
    let vs = shader(&mut c, GL_VERTEX_SHADER, GOOD_VS);
    let fs = shader(&mut c, GL_FRAGMENT_SHADER, REFUSED_FS);
    assert_eq!(
        query::get_shaderiv(&c, fs, GL_COMPILE_STATUS),
        GL_FALSE as i32,
        "the fixture must actually be refused, or this test measures nothing"
    );
    let prog = program(&mut c, vs, fs);

    assert!(
        !record::link_program(&mut c, prog),
        "a link over a shader that failed to compile must fail"
    );
    assert_eq!(
        query::get_programiv(&c, prog, GL_LINK_STATUS),
        GL_FALSE as i32,
        "and glGetProgramiv must say so"
    );
    let log = query::program_info_log(&c, prog);
    assert!(
        log.contains("fragment"),
        "the program log names the stage: {log:?}"
    );
    assert!(
        log.contains("bitCount"),
        "and carries the shader's own reason: {log:?}"
    );
    assert_eq!(
        query::get_programiv(&c, prog, GL_INFO_LOG_LENGTH),
        log.len() as i32 + 1
    );

    // Positive control: recompiling the same attachment from source this driver accepts must link, and
    // the link must still reflect the program's uniforms. A refusal proves nothing without a path that
    // otherwise works.
    record::shader_source(&mut c, fs, GOOD_FS);
    record::compile_shader(&mut c, fs);
    assert!(
        record::link_program(&mut c, prog),
        "the same program links once its shader compiles"
    );
    assert_eq!(
        query::get_programiv(&c, prog, GL_LINK_STATUS),
        GL_TRUE as i32
    );
    assert_eq!(query::program_info_log(&c, prog), "");
    assert_eq!(query::get_programiv(&c, prog, GL_INFO_LOG_LENGTH), 0);
    assert_eq!(query::get_programiv(&c, prog, GL_ACTIVE_UNIFORMS), 1);
    assert!(query::uniform_location(&c, prog, "uTint") >= 0);
}

#[test]
fn a_shader_that_was_never_compiled_is_reported_as_such() {
    let mut c = ctx();
    let vs = shader(&mut c, GL_VERTEX_SHADER, GOOD_VS);
    // Sourced but never compiled — a different state from a refused compile, and ES 3.0 §7.1 keeps them
    // distinct: GL_COMPILE_STATUS is false with no info log.
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, GOOD_FS);
    let prog = program(&mut c, vs, fs);

    assert!(!record::link_program(&mut c, prog));
    let log = query::program_info_log(&c, prog);
    assert!(
        log.contains("never compiled"),
        "an uncompiled shader is not a failed one: {log:?}"
    );
    assert!(
        !log.contains("failed to compile"),
        "and must not be reported as one: {log:?}"
    );

    // Positive control: compiling it is the only thing missing.
    record::compile_shader(&mut c, fs);
    assert!(record::link_program(&mut c, prog));
    assert_eq!(query::program_info_log(&c, prog), "");
}

#[test]
fn a_relink_reflects_the_compile_status_at_link_time() {
    let mut c = ctx();
    let vs = shader(&mut c, GL_VERTEX_SHADER, GOOD_VS);
    let fs = shader(&mut c, GL_FRAGMENT_SHADER, GOOD_FS);
    let prog = program(&mut c, vs, fs);
    assert!(record::link_program(&mut c, prog), "the normal path first");

    // Recompiling an attached shader from source the driver refuses must invalidate the NEXT link; the
    // check cannot be a verdict cached from the first one.
    record::shader_source(&mut c, fs, REFUSED_FS);
    record::compile_shader(&mut c, fs);
    assert!(
        !record::link_program(&mut c, prog),
        "a relink after a refused recompile must fail"
    );
    assert_eq!(
        query::get_programiv(&c, prog, GL_LINK_STATUS),
        GL_FALSE as i32
    );
}
