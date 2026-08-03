//! The GskGpu (GTK4 "gl") / ANGLE unblock: a representative GLSL-ES texture-op pair — `#version 320
//! es`, `precision`, `gl_VertexID`, a combined `sampler2D` GLOBAL, and the hard case, a helper function
//! that takes a `sampler2D` PARAMETER — is rejected wholesale by naga's `glsl-in` (FAIL-before) but
//! compiles to real WGSL through [`glsl_to_wgsl`]'s ES-normalize + sampler-split (PASS-after), with the
//! texture and sampler landing at the coordinated bindings the driver binds to. Mirrors the
//! `spirv_split.rs` FAIL-before/PASS-after proof, sourced from GLSL instead of app SPIR-V.
use super::*;

#[test]
fn spirv_frontend_preserves_dim_buffer_for_explicit_lowering() {
    let words = [
        0x0723_0203,
        0x0001_0000,
        0,
        3,
        0, // header
        0x0002_0011,
        1, // OpCapability Shader
        0x0003_000e,
        0,
        1, // OpMemoryModel Logical GLSL450
        0x0003_0016,
        1,
        32, // %1 = OpTypeFloat 32
        0x0009_0019,
        2,
        1,
        5,
        0,
        0,
        0,
        1,
        0, // %2 = OpTypeImage %1 Buffer
    ];
    let module = naga::front::spv::parse_u8_slice(
        bytemuck::cast_slice(&words),
        &naga::front::spv::Options::default(),
    )
    .unwrap();
    assert!(module.types.iter().any(|(_, ty)| matches!(
        ty.inner,
        naga::TypeInner::Image {
            dim: naga::ImageDimension::Buffer,
            ..
        }
    )));
}

#[test]
fn unknown_uniform_error_carries_bounded_original_and_normalized_context() {
    let original = "#version 460\n\
                    uniform float ublend_S3;\n\
                    layout(location=0) out vec4 color;\n\
                    void main() { color = vec4(ublend_S3); }\n";
    let normalized = "#version 460\n\
                      layout(location=0) out vec4 color;\n\
                      void main() {\n\
                          color = vec4(ublend_S3);\n\
                      }\n";
    let mut frontend = naga::front::glsl::Frontend::default();
    let error = frontend
        .parse(
            &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
            normalized,
        )
        .expect_err("the normalized shader deliberately lost its uniform declaration");

    let message = Diagnostic::glsl_message(original, normalized, &error);

    assert!(
        message.contains("UnknownVariable(\"ublend_S3\")"),
        "{message}"
    );
    assert!(
        message.contains("original GLSL context:")
            && message.contains("2 | uniform float ublend_S3;"),
        "{message}"
    );
    assert!(
        message.contains("normalized GLSL context:")
            && message.contains("4 | color = vec4(ublend_S3);"),
        "{message}"
    );
    assert!(
        message.len() <= Diagnostic::GLSL_CONTEXT_LIMIT,
        "NACK diagnostic exceeded its wire-safe context bound"
    );
}

// A GskGpu-shaped vertex shader: computes position from `gl_VertexID` (no position attribute), reads a
// `binding=0` push-constant-style UBO via `push.`, forwards a uv varying.
const GSK_VERT: &str = r#"#version 320 es
precision highp float;
layout(std140, binding = 0) uniform PushConstants { mat4 mvp; vec4 rect; } push;
out vec2 vUV;
void main() {
    int id = gl_VertexID;
    vec2 corner = vec2(float(id & 1), float((id >> 1) & 1));
    vUV = corner;
    gl_Position = push.mvp * vec4(push.rect.xy + corner * push.rect.zw, 0.0, 1.0);
}
"#;

// A GskGpu-shaped fragment shader: a combined `sampler2D` global sampled THROUGH a helper that takes a
// `sampler2D` parameter — the construct the spec calls a hard naga limit.
const GSK_FRAG: &str = r#"#version 320 es
precision highp float;
uniform sampler2D uTexture;
in vec2 vUV;
layout(location = 0) out vec4 outColor;
vec4 gsk_texture(sampler2D tex, vec2 p) {
    return texture(tex, p);
}
void main() {
    outColor = gsk_texture(uTexture, vUV);
}
"#;

fn naga_direct(src: &str, stage: naga::ShaderStage) -> Result<()> {
    let mut f = naga::front::glsl::Frontend::default();
    f.parse(&naga::front::glsl::Options::from(stage), src)
        .map(|_| ())
        .map_err(|e| Diagnostic::kernel(format!("{e:?}")))
}

#[test]
fn gskgpu_pair_fails_naga_directly_but_compiles_through_glsl_to_wgsl() {
    // FAIL-BEFORE: naga's glsl-in rejects both stages as-is (ES version + gl_VertexID; combined
    // sampler global + sampler2D parameter).
    assert!(
        naga_direct(GSK_VERT, naga::ShaderStage::Vertex).is_err(),
        "vert must fail raw naga"
    );
    assert!(
        naga_direct(GSK_FRAG, naga::ShaderStage::Fragment).is_err(),
        "frag must fail raw naga"
    );

    // PASS-AFTER: the ES-normalize + sampler-split route compiles both to real WGSL.
    let vwgsl = glsl_to_wgsl(GSK_VERT, naga::ShaderStage::Vertex, "vmain")
        .expect("GskGpu vertex must compile through the ES route");
    let fwgsl = glsl_to_wgsl(GSK_FRAG, naga::ShaderStage::Fragment, "fmain")
        .expect("GskGpu fragment must compile through the ES route");

    // Vertex: gl_VertexID lowered to the vertex-index builtin, entry renamed.
    assert!(
        vwgsl.contains("vertex_index"),
        "vertex_index builtin expected: {vwgsl}"
    );
    assert!(vwgsl.contains("vmain"), "entry rename expected: {vwgsl}");

    // Fragment: the combined sampler became a SEPARATE texture_2d + sampler at the coordinated bindings
    // (sampler 0 → guest texture/sampler 1/2 → native 2/3 after the host viewport reservation).
    assert!(
        fwgsl.contains("texture_2d"),
        "expected a separate texture_2d: {fwgsl}"
    );
    assert!(
        fwgsl.contains(": sampler"),
        "expected a separate sampler: {fwgsl}"
    );
    assert!(
        fwgsl.contains("@binding(2)"),
        "texture must reflect at native binding 2: {fwgsl}"
    );
    assert!(
        fwgsl.contains("@binding(3)"),
        "sampler must reflect at native binding 3: {fwgsl}"
    );
    assert!(
        fwgsl.contains("textureSample"),
        "the helper's texture() lowered to a sample: {fwgsl}"
    );
}

// A vertex program that reproduces the *structural* GskGpu constructs the live GTK4 source hit past the
// sampler/version gate: the `#if __VERSION__`-gated UBO binding, `gl_VertexID` hidden in the
// `GSK_VERTEX_INDEX` macro, the location-dropping `IN`/`PASS` macros, a returning `switch`, and — the
// two naga *module* passes — a forward-declared function called before its definition, and a
// value-returning `if/else if` helper with no final `else`.
const GSK_REAL_VERT: &str = r#"#version 320 es
#define GSK_GLES 1
void main_clip_none (void);
precision highp float;
#if __VERSION__ < 420 || (defined(GSK_GLES) && __VERSION__ < 310)
layout(std140)
#else
layout(std140, binding = 0)
#endif
uniform PushConstants { mat4 mvp; } push;
#define GSK_VERTEX_INDEX gl_VertexID
#define IN(_loc) in
#define PASS(_loc) out
IN(0) vec4 in_rect;
IN(1) vec4 in_color;
PASS(0) vec2 _uv;
int classify (uint op)
{
  switch (op)
    {
    case 0u:
      return 1;
    case 1u:
    case 2u:
      return 2;
    default:
      return 0;
    }
}
vec4 pick (int op)
{
  if (op == 1)
    return in_rect;
  else if (op == 2)
    return in_color;
}
void main_clip_none (void)
{
  int c = classify (uint (GSK_VERTEX_INDEX & 3));
  _uv = pick (c).xy;
  gl_Position = push.mvp * (in_rect + in_color);
}
void main ()
{
  main_clip_none ();
}
"#;

#[test]
fn gskgpu_real_structural_constructs_compile_through_glsl_to_wgsl() {
    // FAIL-BEFORE: raw naga rejects the ES version / gl_VertexID outright.
    assert!(
        naga_direct(GSK_REAL_VERT, naga::ShaderStage::Vertex).is_err(),
        "must fail raw naga"
    );

    // PASS-AFTER: the ES lowering + the two module passes (forward-decl reorder, bare-return default)
    // produce a validated WGSL vertex program.
    let wgsl = glsl_to_wgsl(GSK_REAL_VERT, naga::ShaderStage::Vertex, "vmain")
        .expect("real GskGpu structural constructs must compile through the ES route");
    assert!(
        wgsl.contains("vertex_index"),
        "gl_VertexID → vertex_index builtin: {wgsl}"
    );
    assert!(wgsl.contains("vmain"), "entry renamed: {wgsl}");
    // The forward-declared `main_clip_none` and its callees resolved (no forward-dependency).
    assert!(
        wgsl.contains("main_clip_none"),
        "forward-declared fn present: {wgsl}"
    );
}

// The GskGpu border/texture ops declare AGGREGATE interface members naga rejects as a single located
// slot — a `mat3x4` vertex attribute and a `RoundedRect` (== `vec4[3]`) inter-stage varying — and, like
// every GskGpu shader, define `main` (from the shared common.glsl) BEFORE the per-op I/O declarations.
// The real live vertex stop was `EntryPoint { stage: Vertex, source: Argument(1, NotIOShareableType) }`.
const GSK_AGG_VERT: &str = r#"#version 320 es
#define GSK_GLES 1
precision highp float;
#define RoundedRect vec4[3]
#if __VERSION__ < 420 || (defined(GSK_GLES) && __VERSION__ < 310)
layout(std140)
#else
layout(std140, binding = 0)
#endif
uniform PushConstants { mat4 mvp; } push;
#define GSK_VERTEX_INDEX gl_VertexID
#define IN(_loc) in
#define PASS(_loc) out
#define PASS_FLAT(_loc) flat out
void run (out vec2 pos);
void main (void)
{
  vec2 pos;
  run (pos);
  gl_Position = push.mvp * vec4 (pos, 0.0, 1.0);
}
IN(0) mat3x4 in_outline;
IN(3) vec4 in_color;
PASS(0) vec2 _pos;
PASS_FLAT(1) vec4 _color;
PASS_FLAT(2) RoundedRect _outline;
RoundedRect make (mat3x4 m)
{
  return RoundedRect (m[0], m[1], m[2]);
}
void run (out vec2 pos)
{
  RoundedRect o = make (in_outline);
  pos = o[0].xy + float (GSK_VERTEX_INDEX);
  _pos = pos;
  _outline = o;
  _color = in_color;
}
"#;

// The matching fragment stage: the SAME `RoundedRect` varying arrives as a `flat in` array and is read
// through a helper — the input-direction half of the aggregate split (reconstructed at the top of main).
const GSK_AGG_FRAG: &str = r#"#version 320 es
precision highp float;
#define RoundedRect vec4[3]
#define PASS(_loc) in
#define PASS_FLAT(_loc) flat in
PASS(0) vec2 _pos;
PASS_FLAT(1) vec4 _color;
PASS_FLAT(2) RoundedRect _outline;
layout(location = 0) out vec4 out_color;
float coverage (RoundedRect r, vec2 p)
{
  return clamp (r[0].x - p.x, 0.0, 1.0);
}
void main (void)
{
  out_color = _color * coverage (_outline, _pos);
}
"#;

#[test]
fn gskgpu_aggregate_interface_members_split_into_ioshareable_slots() {
    // FAIL-BEFORE: naga rejects `mat3x4` / array interface members at validation (NotIOShareableType).
    // (The parse itself succeeds, so `naga_direct` catches the ES version; validation is the real wall.)
    assert!(
        naga_direct(GSK_AGG_VERT, naga::ShaderStage::Vertex).is_err(),
        "vert must fail raw naga"
    );

    // PASS-AFTER: the `mat3x4` attribute is split into 3 vec4 columns at consecutive locations and the
    // `vec4[3]` varying into 3 flat vec4 slots, each an IO-shareable vector; the aggregates survive as
    // private globals bridged inside `main`, so the whole vertex program validates.
    let vwgsl = glsl_to_wgsl(GSK_AGG_VERT, naga::ShaderStage::Vertex, "vmain")
        .expect("aggregate vertex inputs/varyings must split and compile");
    assert!(vwgsl.contains("vmain"), "entry renamed: {vwgsl}");
    // The matrix input became per-column vector slots (no matrix survives as an entry-point argument).
    assert!(
        vwgsl.contains("in_outline_hlio0"),
        "mat3x4 input split into column slots: {vwgsl}"
    );
    assert!(
        vwgsl.contains("_outline_hlio0"),
        "array varying split into vector slots: {vwgsl}"
    );

    // The fragment stage reads the same split array varying (reconstructed on entry) and compiles.
    let fwgsl = glsl_to_wgsl(GSK_AGG_FRAG, naga::ShaderStage::Fragment, "fmain")
        .expect("aggregate fragment varyings must split and compile");
    assert!(fwgsl.contains("fmain"), "entry renamed: {fwgsl}");
    assert!(
        fwgsl.contains("_outline_hlio0"),
        "array varying split on the fragment side: {fwgsl}"
    );
}

#[test]
fn matrix_array_outputs_split_every_element_and_column() {
    let source = r#"#version 460
layout(location = 0) in vec3 a0;
layout(location = 1) in vec3 a1;
layout(location = 2) in vec3 b0;
layout(location = 3) in vec3 b1;
layout(location = 0) out mat2x3 v_a[1];
layout(location = 2) out mat2x3 v_b[2];
void main() {
    gl_Position = vec4(0.0);
    v_a[0][0] = a0;
    v_a[0][1] = a1;
    v_b[0][0] = b0;
    v_b[1][1] = b1;
}
"#;
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(
            &naga::front::glsl::Options::from(naga::ShaderStage::Vertex),
            source,
        )
        .expect("raw matrix-array GLSL parses before validation");
    assert!(
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .is_err(),
        "matrix arrays must fail validation before aggregate-I/O lowering"
    );
    let wgsl = glsl_to_wgsl(source, naga::ShaderStage::Vertex, "vmain")
        .expect("every matrix-array element and column must become an IO-shareable slot");
    for slot in 0..4 {
        assert!(
            wgsl.contains(&format!("v_b_hlio{slot}")),
            "missing v_b element/column slot {slot}: {wgsl}"
        );
    }
}

// A GLES fragment shader that gates its output color on `isinf()` — the exact Chrome shape that NACKed
// the executor with `Kernel("wgsl-out: Unsupported relational function: IsInf")`.
const ISINF_FRAG: &str = r#"#version 320 es
precision highp float;
in float vScale;
layout(location = 0) out vec4 outColor;
void main() {
    float s = 1.0 / vScale;
    if (isinf(s)) {
        outColor = vec4(1.0, 0.0, 0.0, 1.0);
    } else {
        outColor = vec4(0.0, s, 0.0, 1.0);
    }
}
"#;

#[test]
fn isinf_fails_wgsl_out_directly_but_compiles_through_glsl_to_wgsl() {
    // FAIL-BEFORE: naga's glsl-in ACCEPTS `isinf` (lowering it to a `RelationalFunction::IsInf`), but
    // its `wgsl-out` writer has no emitter for it — validate-then-write NACKs with the IsInf message.
    // (Feed the ES-normalized text so the ONLY remaining gap is the isinf builtin itself.)
    let normalized = crate::glsl_es::Source::new(ISINF_FRAG).normalize(naga::ShaderStage::Fragment);
    let mut f = naga::front::glsl::Frontend::default();
    let mut module = f
        .parse(
            &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
            &normalized,
        )
        .expect("glsl-in accepts isinf");
    match ShaderModule::new(&mut module).wgsl() {
        Err(GpuError::Kernel(m)) => {
            assert!(
                m.contains("IsInf"),
                "expected the IsInf wgsl-out gap, got: {m}"
            )
        }
        other => panic!("expected the IsInf wgsl-out failure, got {other:?}"),
    }

    // PASS-AFTER: the representation-level lowering (`wgsl::nonfinite`) compiles the shader to real WGSL — no `isinf`/`IsInf` survives,
    // and what replaces it is an INTEGER test on the bits (a `bitcast` to `u32`), not a float comparison a
    // fast-math backend could fold away.
    //
    // NOTE what this test does NOT establish, because that was the whole defect: it asserts the shader
    // COMPILES and that no such instruction survives. Neither is the property in question — a rewrite can
    // satisfy both and still answer `false` for every input at runtime. The VALUE is asserted in
    // `tests/nonfinite_predicates.rs`; this one only guards the emission shape.
    let wgsl = glsl_to_wgsl(ISINF_FRAG, naga::ShaderStage::Fragment, "fmain")
        .expect("isinf fragment must compile through the lowering");
    assert!(
        !wgsl.contains("isinf") && !wgsl.contains("isInf") && !wgsl.contains("IsInf"),
        "no isinf survives to WGSL: {wgsl}"
    );
    assert!(
        wgsl.contains("bitcast<u32>"),
        "the predicate is an integer test on the bit pattern, not a float comparison: {wgsl}"
    );
}

#[test]
fn a_vector_argument_to_a_nonfinite_predicate_is_refused_not_mistranslated() {
    // `isinf(vec3)` is legal GLSL returning a `bvec3`. The lowering replaces the predicate with a bitwise
    // AND against a SCALAR mask, and WGSL does not broadcast a scalar across a vector for bitwise
    // operators — so the rebuilt module does not validate and the shader is REFUSED.
    //
    // That is the intended outcome, and it is asserted rather than assumed because the alternative failure
    // mode is the one this codebase keeps finding: a lowering that produces something which compiles and
    // answers wrongly. A refusal names itself; a wrong answer does not. If vector support is ever wanted,
    // the mask has to be splatted to the argument's width, and this test is where that change announces
    // itself.
    let src = "#version 460\nlayout(std140, binding = 0) uniform U { vec4 v; };\nlayout(location=0) out vec4 o;\nvoid main(){ bvec3 b = isinf(v.xyz); o = vec4(b.x ? 1.0 : 0.0); }\n";
    let outcome = glsl_to_wgsl(src, naga::ShaderStage::Fragment, "fmain");
    assert!(
        outcome.is_err(),
        "a vector predicate must be refused, never silently mistranslated: {outcome:?}"
    );
}

#[test]
fn one_dimensional_sample_and_array_index_lower_to_2d() {
    let src = r#"#version 460
layout(set=0,binding=0) uniform texture1DArray t;
layout(set=0,binding=1) uniform sampler s;
layout(location=0) out vec4 o;
void main(){ o = texture(sampler1DArray(t,s), vec2(0.25, 3.0)); }
"#;
    let wgsl = glsl_to_wgsl(src, naga::ShaderStage::Fragment, "fmain")
        .expect("1D-array sample coordinates, gradients, and offsets lower together");
    assert!(wgsl.contains("texture_2d_array"), "{wgsl}");
    assert!(wgsl.contains("textureSample"), "{wgsl}");
}

#[test]
fn one_dimensional_sample_gradient_lowers_to_2d() {
    let src = r#"#version 460
layout(set=0,binding=0) uniform texture1D t;
layout(set=0,binding=1) uniform sampler s;
layout(location=0) out vec4 o;
void main(){ o = textureGrad(sampler1D(t,s), 0.25, 0.1, 0.2); }
"#;
    let wgsl = glsl_to_wgsl(src, naga::ShaderStage::Fragment, "fmain")
        .expect("1D sample coordinate and gradients lower together");
    assert!(wgsl.contains("texture_2d"), "{wgsl}");
    assert!(wgsl.contains("textureSampleGrad"), "{wgsl}");
}

#[test]
fn one_dimensional_storage_load_store_and_size_lower_to_2d() {
    let src = r#"#version 460
layout(local_size_x=1) in;
layout(rgba8,set=0,binding=0) uniform image1D image;
layout(std430,set=0,binding=1) buffer Out { int width; } out_data;
void main(){ vec4 value = imageLoad(image, 3); imageStore(image, 4, value); out_data.width = imageSize(image); }
"#;
    let wgsl = glsl_to_wgsl(src, naga::ShaderStage::Compute, "cmain")
        .expect("1D storage load/store and scalar size lower together");
    assert!(wgsl.contains("texture_storage_2d"), "{wgsl}");
    assert!(wgsl.contains("textureLoad"), "{wgsl}");
    assert!(wgsl.contains("textureStore"), "{wgsl}");
    assert!(wgsl.contains("textureDimensions"), "{wgsl}");
}

#[test]
fn do_while_continue_keeps_the_condition_in_the_continuing_block() {
    let src = r#"#version 460
layout(location=0) out vec4 color;
void main(){ int i=0; do { if(i==2) continue; color=vec4(1.0); } while(++i<3); }
"#;
    let wgsl = glsl_to_wgsl(src, naga::ShaderStage::Fragment, "fmain")
        .expect("finite do-while with continue");
    let continuing = wgsl.find("continuing {").expect("WGSL continuing block");
    assert!(
        wgsl[continuing..].contains("i ="),
        "condition increment must execute on continue: {wgsl}"
    );
    assert!(wgsl[continuing..].contains("break if"), "{wgsl}");
}
#[test]
fn larger_vector_constructor_is_explicitly_truncated_for_naga() {
    let source = "#version 460\nlayout(location=0) out vec4 color;\nvoid main(){ bvec3 input_value=bvec3(true,false,true); ivec2 value=ivec2(input_value); color=vec4(value,0,1); }";
    let normalized = crate::glsl_es::Source::new(source).truncate_vector_constructors();
    assert!(normalized.contains("ivec2(input_value.xy)"), "{normalized}");
    glsl_to_wgsl(source, naga::ShaderStage::Fragment, "main")
        .unwrap_or_else(|error| panic!("truncated vector constructor was refused: {error}"));
}

#[test]
fn scalar_geometric_overloads_are_lifted_for_naga() {
    for expression in [
        "dot(a,b)",
        "normalize(a)",
        "faceforward(a,b,c)",
        "reflect(a,b)",
        "refract(a,b,c)",
    ] {
        let source = format!("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){{ float a=0.5; float b=0.25; float c=0.75; float value={expression}; color=vec4(value); }}");
        glsl_to_wgsl(&source, naga::ShaderStage::Fragment, "main")
            .unwrap_or_else(|error| panic!("scalar {expression} was refused: {error}"));
    }
}

#[test]
fn deqp_scalar_geometric_constant_spellings_are_lifted_for_naga() {
    for expression in [
        "dot(1.0,1.0)",
        "length(1.0)",
        "distance(1.0,2.0)",
        "normalize(1.0)",
        "faceforward(float(1.0),float(1.0),float(1.0))",
        "reflect(1.0,1.0)",
        "refract(1.0,1.0,0.5)",
    ] {
        let source = format!("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){{ float value={expression}; color=vec4(value); }}");
        glsl_to_wgsl(&source, naga::ShaderStage::Fragment, "main")
            .unwrap_or_else(|error| panic!("dEQP scalar {expression} was refused: {error}"));
    }

    let scalar = "void main(){ float v=dot(float(1.0),float(1.0)); }";
    let lifted = crate::glsl_es::Source::new(scalar).lift_scalar_geometric_builtins();
    assert_eq!(lifted.matches("vec2(").count(), 2, "{lifted}");

    let vector = "void main(){ float v=dot(vec2(1.0),vec2(1.0)); }";
    assert_eq!(
        crate::glsl_es::Source::new(vector).lift_scalar_geometric_builtins(),
        vector,
        "a vector overload must remain byte-identical"
    );
}

#[test]
fn scalar_conversion_selects_fold_in_constant_initializers() {
    let source = "#version 460\nlayout(location=0) out vec4 color;\nconst float a=float(true); const int b=int(true); const bool c=bool(1); const vec4 d=vec4(c,a,float(b),bool(0)); void main(){ color=d; }";
    glsl_to_wgsl(source, naga::ShaderStage::Fragment, "main")
        .unwrap_or_else(|error| panic!("constant scalar conversions were refused: {error}"));
}

#[test]
fn deqp_scalar_to_matrix_bool_spelling_compiles() {
    for matrix in ["mat2", "mat3", "mat4"] {
        let source = format!("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){{ bool input_value=true; {matrix} value={matrix}(input_value); color=vec4(value[0][0]); }}");
        glsl_to_wgsl(&source, naga::ShaderStage::Fragment, "main").unwrap_or_else(|error| {
            panic!("dEQP scalar-to-{matrix} bool conversion was refused: {error}")
        });
    }

    let original = "void main(){ mat2 a=mat2(true); mat3 b=mat3(bool(1)); mat4 c=mat4(1.0); }";
    let converted = crate::glsl_es::Source::new(original).convert_bool_scalar_matrix_constructors();
    assert!(converted.contains("mat2(float(true))"), "{converted}");
    assert!(converted.contains("mat3(float(bool(1)))"), "{converted}");
    assert!(converted.contains("mat4(1.0)"), "{converted}");
}

#[test]
fn deqp_vector_to_scalar_constructor_uses_first_component() {
    for (source_type, value, destination_type) in [
        ("bvec2", "bvec2(true,false)", "float"),
        ("bvec3", "bvec3(true,false,true)", "int"),
        ("ivec4", "ivec4(1)", "bool"),
        ("vec2", "vec2(1)", "float"),
    ] {
        let source = format!("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){{ {source_type} input_value={value}; {destination_type} result={destination_type}(input_value); color=vec4(float(result)); }}");
        glsl_to_wgsl(&source, naga::ShaderStage::Fragment, "main").unwrap_or_else(|error| {
            panic!("dEQP {source_type}-to-{destination_type} conversion was refused: {error}")
        });
    }

    let source = "void main(){ bvec2 input_value=bvec2(true); float result=float(input_value); }";
    let normalized = crate::glsl_es::Source::new(source).select_vector_to_scalar_component();
    assert!(normalized.contains("float(input_value.x)"), "{normalized}");
}

#[test]
fn constant_all_and_any_relations_fold() {
    let source = "#version 460\nlayout(location=0) out vec4 color;\nconst bool a=all(bvec3(true,true,true)); const bool b=any(bvec3(false,true,false)); void main(){ color=vec4(float(a),float(b),0,1); }";
    glsl_to_wgsl(source, naga::ShaderStage::Fragment, "main")
        .unwrap_or_else(|error| panic!("constant all/any was refused: {error}"));
}
