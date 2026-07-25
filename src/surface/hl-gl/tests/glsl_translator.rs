//! Adversarial coverage for the just-rewritten GLSL-ES → naga-acceptable desktop GLSL 460 translator
//! (`adapter/glsl.rs`). The host (naga `glsl-in`) owns the real compile, so we assert the STRUCTURAL
//! invariants of the emitted source (the form naga accepts) + the reflection helpers the pipeline keys on,
//! across varied real shaders and malformed input. Every assertion checks REAL translator output — never
//! "didn't crash".

use hl_gl::adapter::glsl;

// ---------------------------------------------------------------------------------------------------
// desktop-GLSL form invariants: no ES dialect leaks; #version pinned; entry present
// ---------------------------------------------------------------------------------------------------

/// A helper that fails loudly if any ES-dialect token survives into the emitted desktop GLSL.
fn assert_no_es_leaks(src: &str) {
    for banned in [
        "attribute ",
        "varying ",
        "gl_FragColor",
        "texture2D(",
        "#version 300",
        " es\n",
    ] {
        assert!(
            !src.contains(banned),
            "ES dialect token {banned:?} leaked into emitted GLSL:\n{src}"
        );
    }
    assert!(
        src.contains("#version 460"),
        "desktop #version not pinned:\n{src}"
    );
    assert!(src.contains("void main()"), "entry point missing:\n{src}");
}

#[path = "glsl_translator/compute.rs"]
mod compute;
#[path = "glsl_translator/dialect.rs"]
mod dialect;
#[path = "glsl_translator/layout.rs"]
mod layout;
#[path = "glsl_translator/location.rs"]
mod location;
#[path = "glsl_translator/metadata.rs"]
mod metadata;
#[path = "glsl_translator/robustness.rs"]
mod robustness;
#[path = "glsl_translator/sampler.rs"]
mod sampler;
#[path = "glsl_translator/uniform.rs"]
mod uniform;
