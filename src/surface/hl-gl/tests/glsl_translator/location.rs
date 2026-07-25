use super::*;

#[test]
fn skia_bare_in_out_get_sequential_and_name_matched_locations() {
    // The real Chrome/Skia GPU-raster shape: BARE attributes/varyings/output (bound by name), no layout().
    let vs = "#version 300 es\nin highp vec4 fillBounds;\nin highp vec4 locations;\n\
              out highp vec2 vatlasCoord_S0;\nflat out mediump vec4 vcolor_S0;\n\
              void main(){ vatlasCoord_S0 = fillBounds.xy; vcolor_S0 = locations; gl_Position = fillBounds + float(gl_VertexID); }\n";
    let fs = "#version 300 es\nprecision mediump float;\nin highp vec2 vatlasCoord_S0;\n\
              flat in mediump vec4 vcolor_S0;\nout mediump vec4 sk_FragColor;\n\
              void main(){ sk_FragColor = vcolor_S0 + vec4(vatlasCoord_S0, 0.0, 0.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).inject_io_locations();
    // Attributes: sequential, own namespace.
    assert!(
        v.contains("layout(location = 0) in highp vec4 fillBounds;"),
        "{v}"
    );
    assert!(
        v.contains("layout(location = 1) in highp vec4 locations;"),
        "{v}"
    );
    // Varyings: vertex out gets 0,1 in decl order; the `flat` qualifier is preserved.
    assert!(
        v.contains("layout(location = 0) out highp vec2 vatlasCoord_S0;"),
        "{v}"
    );
    assert!(
        v.contains("layout(location = 1) flat out mediump vec4 vcolor_S0;"),
        "{v}"
    );
    // Fragment in varyings NAME-MATCH the vertex out locations (0 and 1), flat preserved.
    assert!(
        f.contains("layout(location = 0) in highp vec2 vatlasCoord_S0;"),
        "{f}"
    );
    assert!(
        f.contains("layout(location = 1) flat in mediump vec4 vcolor_S0;"),
        "{f}"
    );
    // Fragment output: own namespace, location 0.
    assert!(
        f.contains("layout(location = 0) out mediump vec4 sk_FragColor;"),
        "{f}"
    );
}

#[test]
fn explicit_locations_are_preserved_and_reserved() {
    // ANGLE's explicit form: already located — must be left untouched, and its slot reserved so a mixed
    // bare decl skips it.
    let vs = "#version 300 es\nlayout(location = 2) in vec4 aExplicit;\nin vec4 aBare;\n\
              layout(location = 5) out vec2 vExplicit;\nout vec2 vBare;\n\
              void main(){ gl_Position = aExplicit + aBare + float(gl_VertexID); }\n";
    let fs = "#version 300 es\nprecision mediump float;\nlayout(location = 5) in vec2 vExplicit;\n\
              in vec2 vBare;\nout vec4 c;\nvoid main(){ c = vec4(vExplicit, vBare); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).inject_io_locations();
    assert!(
        v.contains("layout(location = 2) in vec4 aExplicit;"),
        "explicit attr preserved: {v}"
    );
    assert!(
        v.contains("layout(location = 0) in vec4 aBare;"),
        "bare attr avoids reserved 2: {v}"
    );
    assert!(
        v.contains("layout(location = 5) out vec2 vExplicit;"),
        "explicit varying preserved: {v}"
    );
    assert!(
        v.contains("layout(location = 0) out vec2 vBare;"),
        "bare varying avoids reserved 5: {v}"
    );
    // fs in matches vs out by name: vExplicit → 5, vBare → 0.
    assert!(f.contains("layout(location = 5) in vec2 vExplicit;"), "{f}");
    assert!(f.contains("layout(location = 0) in vec2 vBare;"), "{f}");
}

#[test]
fn gskgpu_macro_style_in_out_is_byte_identical() {
    // GskGpu declares varyings via IN()/PASS() macros whose #define bodies contain `in`/`out` preceded by
    // layout(location). The macro USES are uppercase (IN(0)); the #define lines must be skipped. No bare
    // depth-0 lowercase in/out → byte-identical.
    let vs = "#version 320 es\n#define IN(_loc) layout(location = _loc) in\n\
              #define PASS(_loc) layout(location = _loc) out\n#define PASS_FLAT(_loc) layout(location = _loc) flat out\n\
              IN(0) vec4 aPos;\nPASS(0) vec2 vUV;\nPASS_FLAT(1) vec4 vColor;\n\
              float helper(in vec2 x){ return x.y; }\nvoid main(){ gl_Position = aPos + float(gl_InstanceID); }\n";
    let fs = "#version 320 es\nprecision highp float;\n#define PASS(_loc) layout(location = _loc) in\n\
              PASS(0) vec2 vUV;\nlayout(location = 0) out vec4 c;\nvoid main(){ c = vec4(vUV, 0.0, 1.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).inject_io_locations();
    assert_eq!(v, vs, "gskgpu macro vs must be byte-identical:\n{v}");
    assert_eq!(f, fs, "gskgpu macro fs must be byte-identical:\n{f}");
}

#[test]
fn prepare_verbatim_program_wraps_uniforms_and_injects_locations_together() {
    let vs = "#version 300 es\nuniform highp vec4 sk_RTAdjust;\nin highp vec4 pos;\nout highp vec2 vUV;\n\
              void main(){ vUV = pos.xy; gl_Position = pos * sk_RTAdjust + float(gl_VertexID); }\n";
    let fs = "#version 300 es\nprecision mediump float;\nin highp vec2 vUV;\nout vec4 c;\n\
              void main(){ c = vec4(vUV, 0.0, 1.0); }\n";
    let combined = glsl::StageSources::new(vs, fs).uniform_decls();
    let (v, f) = glsl::prepare_verbatim_program(vs, fs, &combined);
    // Uniform wrapped AND locations injected in one pass.
    assert!(
        v.contains("layout(std140, binding = 0) uniform HlUniforms"),
        "{v}"
    );
    assert!(
        !v.contains("uniform highp vec4 sk_RTAdjust;"),
        "bare uniform removed: {v}"
    );
    assert!(v.contains("layout(location = 0) in highp vec4 pos;"), "{v}");
    assert!(
        v.contains("layout(location = 0) out highp vec2 vUV;"),
        "{v}"
    );
    assert!(f.contains("layout(location = 0) in highp vec2 vUV;"), "{f}");
    assert!(f.contains("layout(location = 0) out vec4 c;"), "{f}");
}
