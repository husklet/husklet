//! GLSL ES 1.00 §3.4 preprocessing, driven by the shapes real GLES2 applications emit.
//!
//! glmark2 2023.01 prepends a precision-portability preamble to every stage
//! (`libmatrix/shader-source.cc`) and its `terrain` shaders size uniform arrays with a `#define`. Both were
//! forwarded unexpanded, so the host compiler reported `UnknownVariable("HIGHP_OR_DEFAULT")` and the guest
//! reflection reported `unsupported uniform type MEDIUMP_OR_DEFAULT` / `non-literal array dimension`.

use super::{assert_naga_parses, assert_no_es_leaks};
use hl_gl::adapter::glsl;

/// The exact preamble `ShaderSource::precision_preamble` emits for a fragment stage.
const FRAGMENT_PREAMBLE: &str = "\
#if defined(GL_ES) && defined(GL_FRAGMENT_PRECISION_HIGH)
#define HIGHP_OR_DEFAULT highp
#else
#define HIGHP_OR_DEFAULT
#endif
#if defined(GL_ES)
#define MEDIUMP_OR_DEFAULT mediump
#else
#define MEDIUMP_OR_DEFAULT
#endif
#ifdef GL_ES
precision mediump float;
#endif
";

/// glmark2's `conditionals`/`function`/`loop` fragment stage after its `$MAIN$` substitution.
fn glmark2_fragment(body: &str) -> String {
    format!(
        "{FRAGMENT_PREAMBLE}varying vec4 dummy;
uniform MEDIUMP_OR_DEFAULT vec2 uvScale;

void main(void)
{{
    HIGHP_OR_DEFAULT vec2 FragCoord = gl_FragCoord.xy;
    float d = fract(FragCoord.x * FragCoord.y * 0.0001 * uvScale.x);
{body}
    gl_FragColor = vec4(d, d, d, 1.0);
}}
"
    )
}

const GLMARK2_VERTEX: &str = "\
#if defined(GL_ES)
#define HIGHP_OR_DEFAULT highp
#else
#define HIGHP_OR_DEFAULT
#endif
attribute vec3 position;
uniform mat4 ModelViewProjectionMatrix;
varying vec4 dummy;

void main(void)
{
    dummy = vec4(1.0);
    HIGHP_OR_DEFAULT float d = fract(position.x);
    gl_Position = ModelViewProjectionMatrix * vec4(position.x, position.y + d, position.z, 1.0);
}
";

#[test]
fn preprocessing_does_not_expand_identifiers_inside_number_tokens() {
    let vs = "#define e +1\nattribute vec4 position; void main(){ int n=1e; gl_Position=position; }";
    let fs = "void main(){ gl_FragColor=vec4(1); }";
    let (translated, _) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(translated.contains("int n=1e;"), "{translated}");
    assert!(!translated.contains("1+1"), "{translated}");
    let mut frontend = naga::front::glsl::Frontend::default();
    assert!(
        frontend
            .parse(
                &naga::front::glsl::Options::from(naga::ShaderStage::Vertex),
                &translated
            )
            .is_err(),
        "the invalid pp-number must reach lexical validation: {translated}"
    );
}

/// The defect: a `#define`d precision qualifier must be expanded before reflection, so no macro identifier
/// survives into the emitted desktop GLSL and the host compiler accepts both stages.
#[test]
fn glmark2_precision_preamble_compiles_on_both_stages() {
    for body in [
        "    if (d < 0.5) { d = 0.5; }",
        "    for (int i = 0; i < 5; i++) d = fract(3.0 * d);",
        "    d = fract(d * 2.0);",
    ] {
        let fragment = glmark2_fragment(body);
        glsl::StageSources::new(GLMARK2_VERTEX, &fragment)
            .uniform_layout()
            .unwrap_or_else(|error| panic!("uniform layout rejected the preamble: {error}"));
        let (vertex, fragment) =
            glsl::StageSources::new(GLMARK2_VERTEX, &fragment).translate_render();
        for stage in [&vertex, &fragment] {
            assert!(
                !stage.contains("HIGHP_OR_DEFAULT") && !stage.contains("MEDIUMP_OR_DEFAULT"),
                "unexpanded macro survived translation:\n{stage}"
            );
            assert_no_es_leaks(stage);
        }
        assert_naga_parses(&vertex, naga::ShaderStage::Vertex);
        assert_naga_parses(&fragment, naga::ShaderStage::Fragment);
    }
}

/// The reflected uniform interface must come from the EXPANDED source: `uniform MEDIUMP_OR_DEFAULT vec2
/// uvScale;` is a `vec2`, not a uniform of type `MEDIUMP_OR_DEFAULT`.
#[test]
fn macro_precision_qualifier_reflects_the_real_type() {
    let fragment = glmark2_fragment("    d = d;");
    let (uniforms, _) = glsl::StageSources::new(GLMARK2_VERTEX, &fragment)
        .uniform_layout()
        .expect("layout");
    let scale = uniforms
        .iter()
        .find(|uniform| uniform.name == "uvScale")
        .expect("uvScale reflected");
    assert_eq!(scale.ty, "vec2");
    assert_eq!(scale.arr, 0);
}

/// glmark2 `terrain`: `#define MAX_POINT_LIGHTS 1` sizes three uniform arrays, and an inactive
/// `#ifdef USE_FOG` branch must contribute no uniforms at all.
#[test]
fn macro_array_dimension_and_inactive_branch() {
    let fragment = "\
#define MAX_POINT_LIGHTS 2
//#define USE_FOG
uniform vec3 ambientLightColor;
#if MAX_POINT_LIGHTS > 0
uniform vec3 pointLightColor[ MAX_POINT_LIGHTS ];
uniform float pointLightDistance[ MAX_POINT_LIGHTS ];
#endif
#ifdef USE_FOG
uniform vec3 fogColor;
#endif
void main() {
    vec3 c = ambientLightColor;
    for (int i = 0; i < MAX_POINT_LIGHTS; i++) {
        c += pointLightColor[i] * pointLightDistance[i];
    }
    gl_FragColor = vec4(c, 1.0);
}
";
    let (uniforms, _) = glsl::StageSources::new(GLMARK2_VERTEX, fragment)
        .uniform_layout()
        .unwrap_or_else(|error| panic!("terrain-shaped uniforms rejected: {error}"));
    let color = uniforms
        .iter()
        .find(|uniform| uniform.name == "pointLightColor")
        .expect("pointLightColor reflected");
    assert_eq!(color.arr, 2, "the #define must size the array");
    assert!(
        !uniforms.iter().any(|uniform| uniform.name == "fogColor"),
        "an inactive #ifdef branch must not be reflected: {uniforms:?}"
    );
    let (_, translated) = glsl::StageSources::new(GLMARK2_VERTEX, fragment).translate_render();
    assert!(
        translated.contains("vec3 pointLightColor[2];"),
        "regenerated block must carry the folded dimension:\n{translated}"
    );
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

/// An array size may be any integral constant expression (GLSL ES 1.00 §4.1.9), including a global
/// `const int` and arithmetic over it — not only a literal.
#[test]
fn const_integer_expression_sizes_an_array() {
    let fragment = "\
const int TAPS = 3;
uniform vec4 kernel[TAPS * 2 + 1];
void main() { gl_FragColor = kernel[0]; }
";
    let (uniforms, _) = glsl::StageSources::new(GLMARK2_VERTEX, fragment)
        .uniform_layout()
        .unwrap_or_else(|error| panic!("const-sized array rejected: {error}"));
    assert_eq!(
        uniforms
            .iter()
            .find(|uniform| uniform.name == "kernel")
            .map(|uniform| uniform.arr),
        Some(7)
    );
}

/// A genuinely non-constant dimension is still rejected, and by the array rule rather than by accident.
#[test]
fn non_constant_array_dimension_is_rejected() {
    let fragment = "\
uniform int count;
uniform vec4 kernel[count];
void main() { gl_FragColor = kernel[0]; }
";
    let error = glsl::StageSources::new(GLMARK2_VERTEX, fragment)
        .uniform_layout()
        .expect_err("a non-constant dimension must be rejected");
    assert_eq!(
        error.to_string(),
        "uniform `kernel` has a non-literal array dimension"
    );
}

/// A function-like macro, `#undef`, `#elif`, and the predefined `GL_ES`/`__VERSION__` macros.
#[test]
fn function_like_macros_and_conditional_chain() {
    let fragment = "\
#define SCALE(v, k) ((v) * float(k))
#define LEVEL 2
#if LEVEL == 1
uniform vec4 unused;
#elif LEVEL == 2 && defined(GL_ES) && __VERSION__ == 100
uniform vec4 base;
#else
#error unreachable
#endif
#define BIAS 4
#undef BIAS
void main() { gl_FragColor = SCALE(base, 3); }
";
    let (uniforms, _) = glsl::StageSources::new(GLMARK2_VERTEX, fragment)
        .uniform_layout()
        .unwrap_or_else(|error| panic!("conditional chain rejected: {error}"));
    assert_eq!(
        uniforms
            .iter()
            .map(|uniform| uniform.name.as_str())
            .collect::<Vec<_>>(),
        ["ModelViewProjectionMatrix", "base"]
    );
    let (_, translated) = glsl::StageSources::new(GLMARK2_VERTEX, fragment).translate_render();
    assert!(
        translated.contains("((base) * float(3))"),
        "function-like macro must expand at the use site:\n{translated}"
    );
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

/// dEQP uses both a `defined` operator produced by an object macro and shallow unary/shift expressions.
/// Neither shape is deeply nested source, and both must select the live branch.
#[test]
fn expanded_defined_and_unary_shift_conditions() {
    for (source, selected) in [
        (
            "#define HAS_MISSING defined(MISSING)\n#if !HAS_MISSING\nint live;\n#endif\n",
            "int live;",
        ),
        (
            "#if !((~2 >> 1) & 1)\nint live;\n#endif\n",
            "int live;",
        ),
        (
            "#if !((~(- - - - - 1 + + + + + +1) >> 1) & 1)\nint first;\n#else\nint second;\n#endif\n",
            "int second;",
        ),
    ] {
        let preprocessed = glsl::Source::new(source)
            .preprocessed()
            .unwrap_or_else(|error| panic!("condition rejected for {source:?}: {error}"));
        assert!(preprocessed.contains(selected), "{preprocessed}");
    }
}

/// A rejected construct must produce an attributed diagnostic naming the line and the construct — never a
/// silent pass-through that fails later in the host compiler.
#[test]
fn unsupported_constructs_report_the_line_and_reason() {
    for (source, expected) in [
        (
            "void main() {}\n#foo bar\n",
            "preprocessor error at line 2: unsupported preprocessor directive `#foo`",
        ),
        (
            "#if 1\nvoid main() {}\n",
            "preprocessor error at line 2: `#if` without a matching `#endif`",
        ),
        (
            "#else\nvoid main() {}\n",
            "preprocessor error at line 1: `#else` without a matching `#if`",
        ),
        (
            "#define A B ## C\nvoid main() {}\n",
            "preprocessor error at line 1: macro `A` uses `#`/`##`, which GLSL ES does not define",
        ),
        (
            "#define F(a) (a)\nvoid main() { float x = F; }\n",
            "preprocessor error at line 2: macro `F` needs an argument list closing on the same line",
        ),
        (
            "#define F(a) (a)\nvoid main() { float x = F(1, 2); }\n",
            "preprocessor error at line 2: macro `F` expects 1 argument(s) but 2 were given",
        ),
        (
            "#if 1 +\n#endif\nvoid main() {}\n",
            "preprocessor error at line 1: `1 +` is not an integral constant expression",
        ),
        (
            "#error needs GL_ES 3\nvoid main() {}\n",
            "preprocessor error at line 1: #error needs GL_ES 3",
        ),
    ] {
        let error = glsl::StageSources::new(GLMARK2_VERTEX, source)
            .uniform_layout()
            .expect_err("the construct must be rejected");
        assert_eq!(error.to_string(), expected, "for source:\n{source}");
    }
}

/// A self-referential macro must not loop forever: GLSL ES 1.00 §3.4 stops rescanning a macro inside its own
/// replacement list, leaving the name in place.
#[test]
fn self_referential_macro_stops_rescanning() {
    let preprocessed = glsl::Source::new("#define A A + 1\nint x = A;\n")
        .preprocessed()
        .expect("preprocess");
    assert!(preprocessed.contains("int x = A + 1;"), "{preprocessed}");
}

/// `validate_sampler_array_uses` is the first gate `Program::link` runs, so it must be the one that reports a
/// preprocessor failure — otherwise unexpanded source reaches reflection.
#[test]
fn the_first_link_gate_rejects_unpreprocessable_source() {
    let error = glsl::StageSources::new(GLMARK2_VERTEX, "#if 1\nvoid main() {}\n")
        .validate_sampler_array_uses()
        .expect_err("the link gate must reject");
    assert_eq!(
        error.to_string(),
        "preprocessor error at line 2: `#if` without a matching `#endif`"
    );
}

/// `#version`/`#extension` carry meaning for whoever consumes the preprocessed text (the host translator
/// keys its ES normalisation on `#version … es`), so preprocessing must not consume them.
#[test]
fn version_and_extension_survive_preprocessing() {
    let source = "\
#version 300 es
#extension GL_OES_EGL_image_external : require
#define TAIL 1
#if TAIL
out vec4 color;
#endif
void main() { color = vec4(1.0); }
";
    let preprocessed = glsl::Source::new(source)
        .preprocessed()
        .expect("preprocess");
    assert!(preprocessed.contains("#version 300 es"), "{preprocessed}");
    assert!(
        preprocessed.contains("#extension GL_OES_EGL_image_external : require"),
        "{preprocessed}"
    );
    assert!(!preprocessed.contains("#define"), "{preprocessed}");
    assert!(preprocessed.contains("out vec4 color;"), "{preprocessed}");
}

/// Line numbering must survive: a directive, a skipped branch and a block comment each keep their lines so a
/// reported line number still matches the application's source.
#[test]
fn line_numbering_is_preserved() {
    let source = "\
#define A 1
/* two
   lines */
#ifdef NOPE
dead
#endif
#if 0 +
#endif
";
    let error = glsl::Source::new(source)
        .preprocessed()
        .expect_err("line 7 is not a constant expression");
    assert_eq!(
        error.to_string(),
        "7: `0 +` is not an integral constant expression"
    );
    let good = glsl::Source::new("#define A 1\nint x = __LINE__;\n")
        .preprocessed()
        .expect("preprocess");
    assert!(good.contains("int x = 2;"), "{good}");
}

/// The four glmark2 2023.01 scenes that shader translation killed, from their REAL upstream sources with the
/// exact runtime preamble prepended (`tests/fixtures/glmark2`). Before preprocessing existed, `conditionals`,
/// `function` and `loop` were rejected by the host `glsl-in` with `UnknownVariable("HIGHP_OR_DEFAULT")` and
/// `terrain` failed reflection with `unsupported uniform type MEDIUMP_OR_DEFAULT` / a non-literal dimension.
#[test]
fn the_four_failing_glmark2_scenes_translate_and_compile() {
    for scene in ["conditionals", "function", "loop", "terrain"] {
        let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/glmark2");
        let vertex = std::fs::read_to_string(format!("{directory}/{scene}.vert"))
            .unwrap_or_else(|error| panic!("{scene}.vert: {error}"));
        let fragment = std::fs::read_to_string(format!("{directory}/{scene}.frag"))
            .unwrap_or_else(|error| panic!("{scene}.frag: {error}"));
        let sources = || glsl::StageSources::new(&vertex, &fragment);
        sources()
            .validate_sampler_array_uses()
            .unwrap_or_else(|error| panic!("{scene}: link gate rejected: {error}"));
        sources()
            .uniform_layout()
            .unwrap_or_else(|error| panic!("{scene}: uniform layout rejected: {error}"));
        let (vertex, fragment) = sources().translate_render();
        for (stage, source) in [("vertex", &vertex), ("fragment", &fragment)] {
            assert!(
                !source.contains("_OR_DEFAULT"),
                "{scene} {stage}: unexpanded macro survived:\n{source}"
            );
            assert_no_es_leaks(source);
        }
        assert_naga_parses(&vertex, naga::ShaderStage::Vertex);
        assert_naga_parses(&fragment, naga::ShaderStage::Fragment);
    }
}

/// A guest must never be able to fault the driver with shader source. Every case here is malformed,
/// adversarial, or unbounded; each must come back as a rejection or a translation, never a panic and never
/// unbounded recursion (a stack overflow faults the process, which is what `terrain` was observed to do).
#[test]
fn hostile_shader_source_is_rejected_not_fatal() {
    let hostile = [
        // Unbalanced and truncated directives.
        "#",
        "#define",
        "#define (",
        "#define A(",
        "#define A(1) x",
        "#undef",
        "#undef 1",
        "#ifdef",
        "#if",
        "#elif 1",
        "#endif",
        "#ifdef A\n#elif 1\n#endif",
        "#ifdef A\n#else\n#elif 1\n#endif",
        // Non-ASCII bytes in and around identifiers and directives.
        "#define \u{e9} 1\nvoid main(){}",
        "void main(){ float \u{e9} = 1.0; }",
        "#if \u{e9}\n#endif\nvoid main(){}",
        // Self- and mutually recursive macros.
        "#define A A\nvoid main(){ A }",
        "#define A B\n#define B A\nvoid main(){ A }",
        "#define F(a) F(a)\nvoid main(){ F(1) }",
        "#define F(a) G(a)\n#define G(a) F(a)\nvoid main(){ F(1) }",
        // Exponential expansion must hit the depth cap rather than the allocator.
        "#define A0 1\n#define A1 A0 A0\n#define A2 A1 A1\n#define A3 A2 A2\n#define A4 A3 A3\nvoid main(){ A4 }",
        // Deeply nested constant expressions and array dimensions.
        "#if ((((((((((((((((((((((((((((((((((((((((1))))))))))))))))))))))))))))))))))))))))\n#endif\nvoid main(){}",
        "uniform vec4 v[((((((((((((((((((((((((((((((((((((((((1))))))))))))))))))))))))))))))))))))))))];\nvoid main(){}",
        "uniform vec4 v[1\nvoid main(){}",
        "uniform vec4 v[];\nvoid main(){}",
        "uniform vec4 v[-1];\nvoid main(){}",
        "uniform vec4 v[0];\nvoid main(){}",
        "uniform vec4 v[1/0];\nvoid main(){}",
        // Unterminated comments and blocks.
        "/* unterminated\nvoid main(){}",
        "void main(){ /* nested /* still one */ }",
        "void main(){",
        "",
    ];
    for source in hostile {
        // Both link gates, then the translation itself: none may panic, and either stage may be rejected.
        let sources = || glsl::StageSources::new(source, source);
        let _ = sources().validate_sampler_array_uses();
        let _ = sources().uniform_layout();
        let _ = sources().sampler_decls();
        let _ = glsl::Source::new(source).vertex_attrs();
        let _ = glsl::Source::new(source).is_forward_verbatim();
        let (vertex, fragment) = sources().translate_render();
        assert!(
            vertex.starts_with("#version 460") && fragment.starts_with("#version 460"),
            "hostile source must still yield a well-formed shell for {source:?}"
        );
    }
}
