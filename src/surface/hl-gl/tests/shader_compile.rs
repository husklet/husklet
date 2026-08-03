use hl_gl::model::context::GlContext;
use hl_gl::model::glconst::{
    GL_COMPILE_STATUS, GL_FALSE, GL_FRAGMENT_SHADER, GL_TRUE, GL_VERTEX_SHADER,
};
use hl_gl::service::{query, record};

fn compile(context: &mut GlContext, kind: u32, source: &str) -> (i32, String) {
    let shader = record::create_shader(context, kind);
    record::shader_source(context, shader, source);
    record::compile_shader(context, shader);
    (
        query::get_shaderiv(context, shader, GL_COMPILE_STATUS),
        query::shader_info_log(context, shader).to_string(),
    )
}

#[test]
fn es2_rejects_the_complete_invalid_implicit_arithmetic_matrix() {
    let floats = ["float", "vec2", "vec3", "vec4"];
    let integers = ["int", "ivec2", "ivec3", "ivec4"];
    let mut context = GlContext::new();

    for operation in ["+", "-", "*", "/"] {
        for float_type in floats {
            for integer_type in integers {
                for shader_kind in [GL_VERTEX_SHADER, GL_FRAGMENT_SHADER] {
                    for result_type in [float_type, integer_type] {
                        let source = format!(
                            "precision mediump float; precision mediump int; void main() {{ {float_type} a; {integer_type} b; {result_type} c = a {operation} b; }}"
                        );
                        let (status, log) = compile(&mut context, shader_kind, &source);
                        assert_eq!(
                            status, GL_FALSE as i32,
                            "accepted {result_type} = {float_type} {operation} {integer_type}"
                        );
                        assert!(
                            log.contains("implicit conversion"),
                            "missing diagnostic: {log:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn same_basic_kind_arithmetic_remains_accepted() {
    let mut context = GlContext::new();
    for source in [
        "void main(){ float a; float b; float c = a + b; }",
        "void main(){ int a; int b; int c = a / b; }",
        "void main(){ vec3 a; float b; vec3 c = a * b; }",
        "void main(){ ivec4 a; int b; ivec4 c = a - b; }",
    ] {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(status, GL_TRUE as i32, "rejected positive control: {log}");
    }
}

#[test]
fn lexical_shapes_do_not_create_false_implicit_conversion_diagnostics() {
    let mut context = GlContext::new();
    let controls = [
        // Comments are not source tokens.
        "void main(){ float a; float b; /* int fake; a + fake; */ float c=a+b; // a + fake\n }",
        // Constructor and function-call syntax is not a variable declaration or a binary expression.
        "float twice(float x){ return x+x; } void main(){ int i; float f=twice(float(i)); }",
        // The inner integer names shadow the outer float names; both expressions are same-kind in scope.
        "void main(){ float a; float b; float c=a+b; { int a; int b; int c=a+b; } c=a+b; }",
        // Reusing a name after its inner declaration leaves scope must resolve to the outer declaration.
        "void main(){ float a; { int a; int b; int c=a+b; } float b; float c=a+b; }",
        // Compound and nested same-kind expressions remain legal.
        "void main(){ vec3 a; float b; vec3 c=(a*b)+(a/b); c += a; }",
    ];
    for source in controls {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(status, GL_TRUE as i32, "false rejection: {log}\n{source}");
    }
}
