//! A stage the translator cannot find a complete `main` in must be refused at link.
//!
//! The driver does not forward a translated stage verbatim: it reflects the declarations and regenerates
//! the stage around the extracted body of `main`. When the body could not be found — no `main(`, or an
//! opening brace that never closes — the extraction returned the empty string, which is
//! indistinguishable from the legitimately empty body of `void main(){}`. The regenerated stage was
//! therefore `void main() {}`, which the host front end CORRECTLY accepts: it compiles, a pipeline is
//! built, the draw executes, and it writes nothing.
//!
//! That is a wrong render with a clean status at every layer, which is worse than a refused one. It is
//! also the reason an earlier measurement missed this: the host front end was tested in isolation and is
//! genuinely strict, but the translator sits in front of it and hands it something valid.

use super::*;

const VS: &str = "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n";
const FS: &str =
    "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ float s=0.0; \
     for(int i=0;i<8;i++){ s+=0.1; } o=vec4(s); }\n";

fn program_from(c: &mut GlContext, vs: &str, fs: &str) -> u32 {
    let vso = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vso, vs);
    record::compile_shader(c, vso);
    let fso = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fso, fs);
    record::compile_shader(c, fso);
    let program = record::create_program(c);
    record::attach_shader(c, program, vso);
    record::attach_shader(c, program, fso);
    program
}

/// The translated fragment stage a linked program forwards to the host.
fn forwarded_fragment(c: &GlContext, program: u32) -> String {
    let words = c
        .programs
        .program(program)
        .expect("program")
        .fs_ir
        .as_ref()
        .expect("fragment payload");
    hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(words)
        .expect("glsl words")
        .expect("decode")
        .source
}

#[test]
fn a_stage_with_an_unclosed_main_does_not_link() {
    // THE POSITIVE CONTROL FIRST. The intact shader must link and must forward a body with its actual
    // statements in it — otherwise the refusal below would be measuring a translator that emits nothing
    // for everything.
    let mut c = ctx();
    let good = program_from(&mut c, VS, FS);
    assert!(record::link_program(&mut c, good));
    let emitted = forwarded_fragment(&c, good);
    assert!(
        emitted.contains("o=vec4(s)"),
        "the intact shader's body must survive translation: {emitted}"
    );

    // Now remove ONE closing brace. This is invalid GLSL and no conformant driver may link it.
    let at = FS.rfind('}').expect("a closing brace");
    let mut broken = FS.to_string();
    broken.remove(at);

    let program = program_from(&mut c, VS, &broken);
    assert!(
        !record::link_program(&mut c, program),
        "a fragment stage whose main is never closed must not link — it used to link and forward \
         `void main() {{}}`, which draws nothing and reports success"
    );
    assert_eq!(
        query::get_programiv(&c, program, GL_LINK_STATUS),
        GL_FALSE as i32
    );
    let log = query::program_info_log(&c, program);
    assert!(
        log.contains("fragment") && log.contains("main"),
        "the log must name the stage and what is wrong with it: {log:?}"
    );
    assert_eq!(
        query::get_programiv(&c, program, GL_INFO_LOG_LENGTH),
        log.len() as i32 + 1
    );
    // Nothing was regenerated from it, so there is no payload to forward.
    assert!(c.programs.program(program).expect("program").fs_ir.is_none());
}

#[test]
fn a_stage_with_no_main_at_all_does_not_link() {
    let mut c = ctx();
    let program = program_from(
        &mut c,
        VS,
        "#version 300 es\nprecision highp float;\nout vec4 o;\nvec4 helper(){ return vec4(1.0); }\n",
    );
    assert!(!record::link_program(&mut c, program));
    assert!(query::program_info_log(&c, program).contains("fragment"));

    // And the vertex stage is gated by the same rule, named as itself.
    let mut c = ctx();
    let program = program_from(&mut c, "#version 300 es\nin vec2 aPos;\n", FS);
    assert!(!record::link_program(&mut c, program));
    assert!(query::program_info_log(&c, program).contains("vertex"));
}

/// The gate must not fire on shapes that are legal. An empty body is legal GLSL, and a helper whose name
/// merely contains `main` must not be mistaken for the entry point.
#[test]
fn legal_shapes_still_link() {
    let mut c = ctx();
    let empty_body = program_from(
        &mut c,
        "#version 300 es\nin vec2 aPos;\nvoid main(){}\n",
        "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){}\n",
    );
    assert!(
        record::link_program(&mut c, empty_body),
        "`void main(){{}}` is a legal shader; only a body that cannot be FOUND is a refusal"
    );

    let helper_named_main = program_from(
        &mut c,
        VS,
        "#version 300 es\nprecision highp float;\nout vec4 o;\n\
         vec4 mainImage(){ return vec4(1.0); }\nvoid main(){ o = mainImage(); }\n",
    );
    assert!(
        record::link_program(&mut c, helper_named_main),
        "a helper containing the word `main` must not hijack the scan"
    );
    let emitted = forwarded_fragment(&c, helper_named_main);
    assert!(
        emitted.contains("mainImage()"),
        "and the real body must still be the one emitted: {emitted}"
    );
}
