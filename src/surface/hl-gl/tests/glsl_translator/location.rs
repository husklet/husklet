use super::*;

#[test]
fn requested_attribute_locations_override_declaration_order() {
    let vs = "attribute vec2 position;\nattribute vec2 texcoord;\n\
              void main(){ gl_Position = vec4(position + texcoord * 0.0, 0.0, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let bindings =
        std::collections::BTreeMap::from([("position".to_owned(), 5), ("texcoord".to_owned(), 2)]);

    let (vertex, _) = glsl::StageSources::new(vs, fs).translate_render_with(&bindings);

    assert!(vertex.contains("layout(location = 5) in vec2 position;"));
    assert!(vertex.contains("layout(location = 2) in vec2 texcoord;"));
}

#[test]
fn requested_locations_apply_to_bare_verbatim_inputs() {
    let vs = "#version 300 es\nin vec4 position;\nin vec2 texcoord;\n\
              void main(){ gl_Position = position + vec4(texcoord * 0.0, 0.0, 0.0); }\n";
    let fs = "#version 300 es\nout vec4 color;\nvoid main(){ color = vec4(1.0); }\n";
    let bindings =
        std::collections::BTreeMap::from([("position".to_owned(), 7), ("texcoord".to_owned(), 3)]);

    let (vertex, _) = glsl::StageSources::new(vs, fs).inject_io_locations_with(&bindings);

    assert!(vertex.contains("layout(location = 7) in vec4 position;"));
    assert!(vertex.contains("layout(location = 3) in vec2 texcoord;"));
}

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

/// A declaration occupying MORE THAN ONE location must reserve all of them.
///
/// A matrix takes one location per column and an array one per element, so `mat4` takes 4 and
/// `vec4 corners[2]` takes 2. Reserving only the first handed the next declaration a location the
/// previous one was still using; naga rejects that as `BindingCollision`, and the program then links,
/// draws with `GL_NO_ERROR`, paints nothing, and wedges the context. This is what stopped GTK4's GPU
/// renderers rendering at all — `layout(location = 1) in mat2 pair;` is the smallest reproduction.
#[test]
fn a_multi_location_declaration_reserves_every_location_it_occupies() {
    // A bare mat4 attribute occupies 0..=3, so the next attribute must start at 4 — not 1.
    let vs = "#version 300 es\nin mat4 model;\nin vec4 colour;\n\
              void main(){ gl_Position = model * colour; }\n";
    let fs =
        "#version 300 es\nprecision mediump float;\nout vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
    let (v, _) = glsl::StageSources::new(vs, fs).inject_io_locations();
    assert!(v.contains("layout(location = 0) in mat4 model;"), "{v}");
    assert!(
        v.contains("layout(location = 4) in vec4 colour;"),
        "a mat4 occupies four locations, so the next attribute starts at 4: {v}"
    );

    // An array attribute occupies one location per element.
    let vs = "#version 300 es\nin vec4 corners[2];\nin vec4 colour;\n\
              void main(){ gl_Position = corners[0] + colour; }\n";
    let (v, _) = glsl::StageSources::new(vs, fs).inject_io_locations();
    assert!(
        v.contains("layout(location = 0) in vec4 corners[2];"),
        "{v}"
    );
    assert!(
        v.contains("layout(location = 2) in vec4 colour;"),
        "a two-element array occupies two locations: {v}"
    );

    // Varyings share one namespace and must respect spans across BOTH stages.
    let vs = "#version 300 es\nin vec4 p;\nout mat4 xform;\nout vec2 uv;\n\
              void main(){ xform = mat4(1.0); uv = p.xy; gl_Position = p; }\n";
    let fs = "#version 300 es\nprecision mediump float;\nin mat4 xform;\nin vec2 uv;\n\
              out vec4 c;\nvoid main(){ c = xform[0] + vec4(uv, 0.0, 0.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).inject_io_locations();
    assert!(v.contains("layout(location = 0) out mat4 xform;"), "{v}");
    assert!(
        v.contains("layout(location = 4) out vec2 uv;"),
        "a mat4 varying occupies four locations: {v}"
    );
    assert!(f.contains("layout(location = 0) in mat4 xform;"), "{f}");
    assert!(f.contains("layout(location = 4) in vec2 uv;"), "{f}");
}

/// An EXPLICIT location reserves its whole span too, so a bare declaration cannot be assigned into the
/// middle of it. `layout(location = 1) in mat2 pair;` occupies 1 and 2.
#[test]
fn an_explicit_multi_location_declaration_is_not_overlapped() {
    let vs =
        "#version 300 es\nlayout(location = 1) in mat2 pair;\nin vec4 colour;\nin vec4 extra;\n\
              void main(){ gl_Position = vec4(pair[0], 0.0, 0.0) + colour + extra; }\n";
    let fs =
        "#version 300 es\nprecision mediump float;\nout vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
    let (v, _) = glsl::StageSources::new(vs, fs).inject_io_locations();
    // The explicit decl is preserved untouched.
    assert!(v.contains("layout(location = 1) in mat2 pair;"), "{v}");
    // 0 is free, but 1 and 2 are taken by the mat2 — so the bare decls take 0 then 3.
    assert!(v.contains("layout(location = 0) in vec4 colour;"), "{v}");
    assert!(
        v.contains("layout(location = 3) in vec4 extra;"),
        "locations 1 and 2 belong to the mat2: {v}"
    );
}

/// `matCxR` has `C` columns and therefore takes `C` locations regardless of its row count — `mat2x4`
/// takes 2 and `mat4x2` takes 4. Getting this backwards would over- or under-reserve every matrix.
#[test]
fn matrix_location_span_follows_the_column_count_not_the_row_count() {
    let fs =
        "#version 300 es\nprecision mediump float;\nout vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
    for (declaration, span) in [("mat2x4", 2u32), ("mat4x2", 4), ("mat3", 3), ("mat2", 2)] {
        let vs = format!(
            "#version 300 es\nin {declaration} m;\nin vec4 colour;\n\
             void main(){{ gl_Position = vec4(m[0]) + colour; }}\n"
        );
        let (v, _) = glsl::StageSources::new(&vs, fs).inject_io_locations();
        assert!(
            v.contains(&format!("layout(location = {span}) in vec4 colour;")),
            "{declaration} must occupy {span} locations: {v}"
        );
    }
}

/// The ES2/ES3 TRANSLATE path allocates locations too, independently of the verbatim injector, and it
/// must use the same span arithmetic.
///
/// Plain ES3 shaders take this path, not the verbatim one, so a span fix applied only to the injector
/// leaves every ordinary application still overlapping its matrix and array declarations. Because
/// `Program::attrib_locations` is read back off the TRANSLATED source, this also decides what
/// `glGetAttribLocation` reports — emission and reflection cannot disagree, but they can be wrong
/// together.
#[test]
fn the_translate_path_allocates_attribute_locations_by_span() {
    let vs = "#version 300 es\nin vec4 position;\nin mat4 transform;\nin vec4 tint;\n\
              void main(){ gl_Position = transform * position + tint; }\n";
    let fs =
        "#version 300 es\nprecision mediump float;\nout vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
    let (v, _) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(v.contains("layout(location = 0) in vec4 position;"), "{v}");
    assert!(v.contains("layout(location = 1) in mat4 transform;"), "{v}");
    assert!(
        v.contains("layout(location = 5) in vec4 tint;"),
        "a mat4 occupies locations 1..=4, so tint must be 5: {v}"
    );
}

/// Varyings on the translate path were numbered by their ENUMERATE INDEX, which cannot account for a
/// multi-location varying at all. Both stages must walk identically or the inter-stage interface stops
/// matching at the first matrix varying.
#[test]
fn the_translate_path_numbers_varyings_by_span_in_both_stages() {
    let vs = "#version 300 es\nin vec4 p;\nout mat4 xform;\nout vec2 uv;\n\
              void main(){ xform = mat4(1.0); uv = p.xy; gl_Position = p; }\n";
    let fs = "#version 300 es\nprecision mediump float;\nin mat4 xform;\nin vec2 uv;\n\
              out vec4 c;\nvoid main(){ c = xform[0] + vec4(uv, 0.0, 0.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(v.contains("layout(location = 0) out mat4 xform;"), "{v}");
    assert!(
        v.contains("layout(location = 4) out vec2 uv;"),
        "a mat4 varying occupies four locations: {v}"
    );
    // The fragment stage must agree declaration for declaration.
    assert!(f.contains("layout(location = 0) in mat4 xform;"), "{f}");
    assert!(f.contains("layout(location = 4) in vec2 uv;"), "{f}");
}
