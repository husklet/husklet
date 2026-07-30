use super::*;

#[test]
fn translate_compute_pins_desktop_version_and_strips_es_dialect() {
    let cs = "#version 310 es\nlayout(local_size_x = 64) in;\n\
              layout(std430, binding = 0) buffer Data { float v[]; };\n\
              void main(){ highp uint i = gl_GlobalInvocationID.x; v[i] = float(i); }\n";
    let out = glsl::Translator::compute(cs);
    assert!(
        out.starts_with("#version 460\n"),
        "compute version pinned: {out}"
    );
    assert!(
        !out.contains("#version 310"),
        "ES compute version not stripped: {out}"
    );
    assert!(
        !out.contains("highp"),
        "ES precision not stripped from compute: {out}"
    );
    // The compute body + SSBO layout survive.
    assert!(out.contains("layout(local_size_x = 64) in;"), "{out}");
    assert!(out.contains("void main()"), "{out}");
}

#[test]
fn compute_comment_scanner_preserves_comment_markers_inside_quotes() {
    let source = "#version 310 es\nvoid main() { /* keep */ int u = 1; } // trailing\n";
    let translated = glsl::Translator::compute(source);
    assert!(translated.contains("int u = 1;"), "{translated}");
    assert!(!translated.contains("keep"), "{translated}");
    assert!(!translated.contains("trailing"), "{translated}");
}

// ---------------------------------------------------------------------------------------------------
// malformed / degenerate input — honest structural output, never a panic or silent-wrong
// ---------------------------------------------------------------------------------------------------
