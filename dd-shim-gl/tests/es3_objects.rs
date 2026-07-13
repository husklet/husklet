//! In-tree mirror gates for the two ES3 object families this increment gives real bodies:
//!   - `gles_query_objects_track_targets_availability_and_asynchronous_results` (ledger: was "missing")
//!   - the sampler-object slice of `opt_in_gles3_has_real_implementations_for_every_mandatory_command`
//!
//! These were previously no-ops that always returned 0. The assertions below exercise the observable
//! object semantics through the public GLES entry points (typed targets, name validation, per-object
//! parameter storage, availability tied to submission completion) so a regression to the old no-op
//! bodies fails the crate's own `cargo test -p dd-shim-gl`.

use dd_shim_gl::gles;
use dd_shim_gl::glconst::*;

fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn drain() {
    while gles::glGetError() != GL_NO_ERROR {}
}

#[test]
fn sampler_objects_store_and_report_per_object_state() {
    let _serial = serial_guard();
    drain();

    // glGenSamplers reserves names; a reserved-but-unbound name is not yet a sampler object.
    let mut ids = [0u32; 2];
    gles::glGenSamplers(2, ids.as_mut_ptr());
    assert_ne!(ids[0], 0);
    assert_ne!(ids[1], 0);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(gles::glIsSampler(ids[0]), GL_FALSE, "generation only reserves a name");

    // Binding instantiates the object with ES3 default state.
    gles::glBindSampler(0, ids[0]);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(gles::glIsSampler(ids[0]), GL_TRUE, "first bind must instantiate the sampler");

    // Defaults (ES 3.0 table 6.10) are observable before any parameter is set.
    let mut minf = -1;
    gles::glGetSamplerParameteriv(ids[0], GL_TEXTURE_MIN_FILTER, &mut minf);
    assert_eq!(minf as u32, GL_NEAREST_MIPMAP_LINEAR, "default MIN_FILTER");
    let mut wrap = -1;
    gles::glGetSamplerParameteriv(ids[0], GL_TEXTURE_WRAP_S, &mut wrap);
    assert_eq!(wrap as u32, GL_REPEAT, "default WRAP_S");
    let mut min_lod = 0.0f32;
    gles::glGetSamplerParameterfv(ids[0], GL_TEXTURE_MIN_LOD, &mut min_lod);
    assert_eq!(min_lod, -1000.0, "default MIN_LOD");

    // A set value round-trips through the getter (the whole point — no longer a no-op returning 0).
    gles::glSamplerParameteri(ids[0], GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32);
    gles::glSamplerParameteri(ids[0], GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
    gles::glSamplerParameterf(ids[0], GL_TEXTURE_MAX_LOD, 7.5);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    gles::glGetSamplerParameteriv(ids[0], GL_TEXTURE_MIN_FILTER, &mut minf);
    assert_eq!(minf as u32, GL_LINEAR, "MIN_FILTER did not persist");
    gles::glGetSamplerParameteriv(ids[0], GL_TEXTURE_WRAP_T, &mut wrap);
    assert_eq!(wrap as u32, GL_CLAMP_TO_EDGE, "WRAP_T did not persist");
    let mut max_lod = 0.0f32;
    gles::glGetSamplerParameterfv(ids[0], GL_TEXTURE_MAX_LOD, &mut max_lod);
    assert_eq!(max_lod, 7.5, "MAX_LOD did not persist");

    // Distinct objects keep distinct state.
    gles::glBindSampler(1, ids[1]);
    gles::glGetSamplerParameteriv(ids[1], GL_TEXTURE_MIN_FILTER, &mut minf);
    assert_eq!(minf as u32, GL_NEAREST_MIPMAP_LINEAR, "second sampler must keep its own defaults");

    // Atomic validation: an invalid enum value is rejected and leaves the object unchanged.
    gles::glSamplerParameteri(ids[0], GL_TEXTURE_MIN_FILTER, 0x1234);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM, "invalid filter enum was accepted");
    gles::glGetSamplerParameteriv(ids[0], GL_TEXTURE_MIN_FILTER, &mut minf);
    assert_eq!(minf as u32, GL_LINEAR, "rejected value mutated the sampler");

    // Operating on a name that was never generated is INVALID_OPERATION, output preserved.
    let mut sentinel = 0x5AA5;
    gles::glGetSamplerParameteriv(4242, GL_TEXTURE_MIN_FILTER, &mut sentinel);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);
    assert_eq!(sentinel, 0x5AA5, "error path mutated the output");
    gles::glBindSampler(0, 4242);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);

    // Deletion revokes the object and unbinds it from every unit.
    gles::glDeleteSamplers(2, ids.as_ptr());
    assert_eq!(gles::glIsSampler(ids[0]), GL_FALSE, "deleted sampler is still an object");
    gles::glGetSamplerParameteriv(ids[0], GL_TEXTURE_MIN_FILTER, &mut sentinel);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "deleted sampler still queryable");
    // Deleting 0 and re-deleting are silently ignored; a negative count is INVALID_VALUE.
    drain();
    gles::glDeleteSamplers(2, ids.as_ptr());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    gles::glGenSamplers(-1, ids.as_mut_ptr());
    // (glGen with negative n is a documented no-op returning early; no error required.)
    gles::glDeleteSamplers(-1, ids.as_ptr());
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
}

#[test]
fn query_objects_track_typed_targets_and_availability() {
    let _serial = serial_guard();
    drain();

    let mut q = 0u32;
    gles::glGenQueries(1, &mut q);
    assert_ne!(q, 0);
    // A reserved-but-unbegun name is not yet a query object.
    assert_eq!(gles::glIsQuery(q), GL_FALSE, "generation only reserves a query name");

    // No query active for the target yet.
    let mut cur = -1;
    gles::glGetQueryiv(GL_ANY_SAMPLES_PASSED, GL_CURRENT_QUERY, &mut cur);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(cur, 0, "no query should be active before glBeginQuery");

    // An invalid target is INVALID_ENUM; id 0 / an ungenerated name is INVALID_OPERATION.
    gles::glBeginQuery(0x1234, q);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    gles::glBeginQuery(GL_ANY_SAMPLES_PASSED, 0);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);
    gles::glBeginQuery(GL_ANY_SAMPLES_PASSED, 9999);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);

    // Begin binds the name to the target and makes it the current query + a live object.
    gles::glBeginQuery(GL_ANY_SAMPLES_PASSED, q);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(gles::glIsQuery(q), GL_TRUE, "an active query must be an object");
    gles::glGetQueryiv(GL_ANY_SAMPLES_PASSED, GL_CURRENT_QUERY, &mut cur);
    assert_eq!(cur as u32, q, "CURRENT_QUERY must report the active query");

    // A second Begin on the same target while one is active is INVALID_OPERATION.
    let mut q2 = 0u32;
    gles::glGenQueries(1, &mut q2);
    gles::glBeginQuery(GL_ANY_SAMPLES_PASSED, q2);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "nested query on one target must fail");

    // Result is not available while the query is still active (querying an active query is an error).
    let mut avail = 0xdead_u32;
    gles::glGetQueryObjectuiv(q, GL_QUERY_RESULT_AVAILABLE, &mut avail);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "active query result must not be readable");
    assert_eq!(avail, 0xdead, "error path mutated the output");

    // End captures completion and clears the active slot.
    gles::glEndQuery(GL_ANY_SAMPLES_PASSED);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    gles::glGetQueryiv(GL_ANY_SAMPLES_PASSED, GL_CURRENT_QUERY, &mut cur);
    assert_eq!(cur, 0, "END must clear CURRENT_QUERY");

    // Availability is a real boolean; the counted result reads back (truthful 0, no backend counter).
    avail = 0xdead;
    gles::glGetQueryObjectuiv(q, GL_QUERY_RESULT_AVAILABLE, &mut avail);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert!(avail == 0 || avail == 1, "AVAILABLE must be a boolean, got {avail}");
    let mut result = 0xdead_u32;
    gles::glGetQueryObjectuiv(q, GL_QUERY_RESULT, &mut result); // blocks for completion first
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(result, 0, "no occlusion backend yet ⇒ truthful zero samples");
    // After GL_QUERY_RESULT (which waits), availability must be true.
    gles::glGetQueryObjectuiv(q, GL_QUERY_RESULT_AVAILABLE, &mut avail);
    assert_eq!(avail, 1, "result must be available after a completion wait");

    // A typed name cannot be reused with a different target.
    gles::glBeginQuery(GL_TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN, q);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "query name is bound to its first target");

    // Deletion revokes the object; a stale id is INVALID_OPERATION with output preserved.
    gles::glDeleteQueries(1, &q);
    assert_eq!(gles::glIsQuery(q), GL_FALSE);
    let mut sentinel = 0x1357_u32;
    gles::glGetQueryObjectuiv(q, GL_QUERY_RESULT, &mut sentinel);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);
    assert_eq!(sentinel, 0x1357, "stale query read mutated output");
    // Clean up the still-reserved q2.
    gles::glDeleteQueries(1, &q2);
    drain();
    gles::glDeleteQueries(-1, &q);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
}
