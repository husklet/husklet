use super::*;

// ===================================================================================================
// service ops (sink-touching / es3 object) hostile edges → GL error, no panic
// ===================================================================================================

/// A grab-bag of hostile object-service calls (bad sampler/query/sync/transform-feedback/pipeline names +
/// enums): each sets the right GL error (or safely no-ops) and never panics; a valid call then works.
#[test]
fn hostile_object_service_edges_never_panic() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    // Sampler: unknown name → INVALID_OPERATION; junk pname → INVALID_ENUM.
    es3::sampler_parameter(&mut c, 9090, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    let s = c.samplers.gen();
    es3::sampler_parameter(&mut c, s, 0xDEAD, 0, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    // Query: bad target → INVALID_ENUM; unknown id → INVALID_OPERATION.
    es3::begin_query(&mut c, 0xDEAD, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, 777);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // Program pipeline: use-stages on unknown pipeline → INVALID_OPERATION; bad stage bits → INVALID_VALUE.
    es3::use_program_stages(&mut c, 4242, GL_VERTEX_SHADER_BIT, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    let pp = c.gen_program_pipeline();
    es3::use_program_stages(&mut c, pp, 0x8000_0000, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // Sync: junk condition/flags → INVALID_ENUM/INVALID_VALUE; waiting/deleting an unknown sync →
    // INVALID_VALUE and never a panic.
    assert!(sync::fence_sync(&mut c, &mut sink, 0xDEAD, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert_eq!(
        sync::client_wait_sync(&mut c, &mut sink, 555, 0, 0),
        GL_WAIT_FAILED
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    sync::wait_sync(&mut c, &mut sink, 555, 0, GL_TIMEOUT_IGNORED);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(sync::get_synciv(&mut c, &mut sink, 555, GL_SYNC_STATUS).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // Transform feedback: bad varyings program → INVALID_VALUE; junk primitive mode → INVALID_ENUM.
    es3::transform_feedback_varyings(&mut c, 0, vec!["v".into()], GL_INTERLEAVED_ATTRIBS);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    c.begin_transform_feedback(0xDEAD);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    // A valid sampler parameter still sticks afterwards.
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// `glGetIntegerv` / `glGetString` / `glGetStringi` with unrecognized names return a benign fallback
/// (never a null deref / OOB) — an app polling junk enums must not crash the driver.
#[test]
fn junk_query_names_return_benign_fallbacks() {
    let c = ctx();
    let mut out = [0i32; 4];
    assert_eq!(query::get_integerv(&c, 0xDEAD_BEEF, &mut out), 1);
    assert_eq!(out[0], 0);
    // An unrecognized glGetString name is the empty (NUL-terminated) string, never null.
    assert_eq!(query::gl_string(0xDEAD), b"\0");
    // An out-of-range indexed extension query is None (the caller returns a null pointer + spec error).
    assert!(query::string_i(GL_EXTENSIONS, 9999).is_none());
    // Reflection getters on a bogus program are -1 / 0 / None, never a panic.
    assert_eq!(query::uniform_location(&c, 777, "x"), -1);
    assert_eq!(query::attrib_location(&c, 777, "x"), -1);
    assert_eq!(query::get_programiv(&c, 777, GL_LINK_STATUS), 0);
    assert!(query::active_uniform(&c, 777, 0).is_none());
}
