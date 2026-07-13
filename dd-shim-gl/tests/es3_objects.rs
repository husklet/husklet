//! In-tree mirror gates for the ES3 object families this cluster gives real bodies:
//!   - `gles_query_objects_track_targets_availability_and_asynchronous_results` (ledger: was "missing")
//!   - sampler / transform-feedback / uniform-block-binding slices of
//!     `opt_in_gles3_has_real_implementations_for_every_mandatory_command`
//!
//! These were previously no-ops that always returned 0. The assertions below exercise the observable
//! object semantics through the public GLES entry points (typed targets, name validation, per-object
//! parameter storage, availability tied to submission completion, indexed binding state) so a
//! regression to the old no-op bodies fails the crate's own `cargo test -p dd-shim-gl`.

use core::ffi::c_char;

use dd_shim_gl::gles;
use dd_shim_gl::glconst::*;

/// A minimally-linked program (enough for the object families below to treat it as a program object).
fn linked_program() -> u32 {
    fn compile(kind: u32, source: &str) -> u32 {
        let shader = gles::glCreateShader(kind);
        let src = std::ffi::CString::new(source).unwrap();
        let ptr = src.as_ptr();
        gles::glShaderSource(shader, 1, &ptr, core::ptr::null());
        gles::glCompileShader(shader);
        shader
    }
    let vs = compile(GL_VERTEX_SHADER, "attribute vec2 p; void main(){ gl_Position=vec4(p,0.0,1.0); }");
    let fs = compile(GL_FRAGMENT_SHADER, "precision mediump float; void main(){ gl_FragColor=vec4(1.0); }");
    let program = gles::glCreateProgram();
    gles::glAttachShader(program, vs);
    gles::glAttachShader(program, fs);
    gles::glLinkProgram(program);
    program
}

fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Reset the shared default GL share-group under the lock (see semantic_gates.rs) for determinism.
    gles::reset_gl_state_for_tests();
    g
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

#[test]
fn transform_feedback_objects_track_typed_lifecycle_and_varyings() {
    let _serial = serial_guard();
    drain();

    // The default object (name 0) is bound and idle; 0 is not itself a transform-feedback object.
    assert_eq!(gles::transform_feedback_state(), (0, false, false));
    assert_eq!(gles::glIsTransformFeedback(0), GL_FALSE);

    // Generation reserves a name (not yet an object); binding instantiates it.
    let mut tf = 0u32;
    gles::glGenTransformFeedbacks(1, &mut tf);
    assert_ne!(tf, 0);
    assert_eq!(gles::glIsTransformFeedback(tf), GL_FALSE, "generation only reserves a name");
    gles::glBindTransformFeedback(0x1234, tf);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM, "bad bind target must be rejected");
    gles::glBindTransformFeedback(GL_TRANSFORM_FEEDBACK, tf);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(gles::glIsTransformFeedback(tf), GL_TRUE, "first bind must instantiate the object");
    assert_eq!(gles::transform_feedback_state(), (tf, false, false));

    // begin/end/pause/resume state machine.
    gles::glBeginTransformFeedback(0x1234);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM, "bad primitive mode must be rejected");
    gles::glBeginTransformFeedback(GL_TRIANGLES);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(gles::transform_feedback_state(), (tf, true, false));
    gles::glBeginTransformFeedback(GL_TRIANGLES);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "nested begin must fail");

    // Cannot rebind while active and not paused.
    let mut tf2 = 0u32;
    gles::glGenTransformFeedbacks(1, &mut tf2);
    gles::glBindTransformFeedback(GL_TRANSFORM_FEEDBACK, tf2);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "rebind while active must fail");

    gles::glPauseTransformFeedback();
    assert_eq!(gles::transform_feedback_state(), (tf, true, true));
    gles::glPauseTransformFeedback();
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "double pause must fail");
    gles::glResumeTransformFeedback();
    assert_eq!(gles::transform_feedback_state(), (tf, true, false));
    gles::glResumeTransformFeedback();
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "resume while running must fail");

    // Deleting an active object is an error; ending first allows deletion + reverts to the default.
    gles::glDeleteTransformFeedbacks(1, &tf);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "delete of active TF must fail");
    gles::glEndTransformFeedback();
    assert_eq!(gles::transform_feedback_state(), (tf, false, false));
    gles::glEndTransformFeedback();
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "end while inactive must fail");
    gles::glDeleteTransformFeedbacks(1, &tf);
    assert_eq!(gles::glIsTransformFeedback(tf), GL_FALSE, "deleted TF is still an object");
    assert_eq!(gles::transform_feedback_state().0, 0, "deleting the bound TF reverts to the default");
    // Default cannot be deleted; a negative count is INVALID_VALUE.
    drain();
    let zero = 0u32;
    gles::glDeleteTransformFeedbacks(1, &zero);
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "deleting the default TF must be silently ignored");
    gles::glDeleteTransformFeedbacks(-1, &zero);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    gles::glDeleteTransformFeedbacks(1, &tf2);

    // Varying capture list round-trips through glGetTransformFeedbackVarying.
    let program = linked_program();
    let vpos = std::ffi::CString::new("vPos").unwrap();
    let vnorm = std::ffi::CString::new("vNorm").unwrap();
    let names = [vpos.as_ptr(), vnorm.as_ptr()];
    gles::glTransformFeedbackVaryings(program, 2, names.as_ptr(), 0x1234);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM, "bad buffer mode must be rejected");
    gles::glTransformFeedbackVaryings(program, 2, names.as_ptr(), GL_INTERLEAVED_ATTRIBS);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    let mut buf = [0 as c_char; 16];
    let (mut len, mut size, mut typ) = (-1i32, -1i32, 0u32);
    gles::glGetTransformFeedbackVarying(program, 0, 16, &mut len, &mut size, &mut typ, buf.as_mut_ptr());
    let got = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
    assert_eq!(got, "vPos", "captured varying name must round-trip");
    assert_eq!((len, size, typ), (4, 1, GL_FLOAT_VEC4));
    gles::glGetTransformFeedbackVarying(program, 1, 16, &mut len, &mut size, &mut typ, buf.as_mut_ptr());
    let got = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
    assert_eq!(got, "vNorm");
    // Out-of-range index is INVALID_VALUE with outputs zeroed.
    len = -1;
    gles::glGetTransformFeedbackVarying(program, 2, 16, &mut len, &mut size, &mut typ, buf.as_mut_ptr());
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    assert_eq!(len, 0);
}

#[test]
fn uniform_block_binding_and_indexed_buffer_bindings_are_real() {
    let _serial = serial_guard();
    drain();
    let program = linked_program();

    // Block indices are assigned lazily and stably per queried name (default binding 0).
    let blk = std::ffi::CString::new("Blk").unwrap();
    let blk2 = std::ffi::CString::new("Blk2").unwrap();
    let i0 = gles::glGetUniformBlockIndex(program, blk.as_ptr());
    let i1 = gles::glGetUniformBlockIndex(program, blk2.as_ptr());
    assert_eq!(i0, 0);
    assert_eq!(i1, 1);
    assert_eq!(gles::glGetUniformBlockIndex(program, blk.as_ptr()), 0, "block index must be stable");
    // An unknown program has no block namespace.
    assert_eq!(gles::glGetUniformBlockIndex(4242, blk.as_ptr()), GL_INVALID_INDEX);

    // The block name round-trips, and the default binding is 0.
    let mut name = [0 as c_char; 16];
    let mut nlen = -1;
    gles::glGetActiveUniformBlockName(program, 0, 16, &mut nlen, name.as_mut_ptr());
    let got = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_str().unwrap();
    assert_eq!(got, "Blk");
    assert_eq!(nlen, 3);
    let mut binding = -1;
    gles::glGetActiveUniformBlockiv(program, 0, GL_UNIFORM_BLOCK_BINDING, &mut binding);
    assert_eq!(binding, 0, "default uniform-block binding is 0");

    // glUniformBlockBinding sets it; the getter reflects the change.
    gles::glUniformBlockBinding(program, 0, 3);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    gles::glGetActiveUniformBlockiv(program, 0, GL_UNIFORM_BLOCK_BINDING, &mut binding);
    assert_eq!(binding, 3, "uniform-block binding did not persist");
    // NAME_LENGTH is real; an out-of-range block index is INVALID_VALUE.
    let mut name_len = -1;
    gles::glGetActiveUniformBlockiv(program, 0, GL_UNIFORM_BLOCK_NAME_LENGTH, &mut name_len);
    assert_eq!(name_len, 4);
    let mut sentinel = 0x77;
    gles::glGetActiveUniformBlockiv(program, 99, GL_UNIFORM_BLOCK_BINDING, &mut sentinel);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    assert_eq!(sentinel, 0x77, "out-of-range block query mutated output");

    // Indexed buffer binding points (glBindBufferBase / glBindBufferRange). The binding point records
    // the buffer name directly — no generic bind of the ES3-only UNIFORM_BUFFER target is required.
    let mut buf = 0u32;
    gles::glGenBuffers(1, &mut buf);
    drain();
    gles::glBindBufferBase(GL_UNIFORM_BUFFER, 2, buf);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(gles::indexed_buffer_binding(GL_UNIFORM_BUFFER, 2), Some((buf, 0, 0)));
    gles::glBindBufferRange(GL_UNIFORM_BUFFER, 3, buf, 16, 64);
    assert_eq!(gles::indexed_buffer_binding(GL_UNIFORM_BUFFER, 3), Some((buf, 16, 64)));
    // Binding buffer 0 clears the index.
    gles::glBindBufferBase(GL_UNIFORM_BUFFER, 2, 0);
    assert_eq!(gles::indexed_buffer_binding(GL_UNIFORM_BUFFER, 2), None);

    // Validation: bad target, out-of-range index, and a bad range are rejected.
    gles::glBindBufferBase(0x1234, 0, buf);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    gles::glBindBufferBase(GL_UNIFORM_BUFFER, 99, buf);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    gles::glBindBufferRange(GL_UNIFORM_BUFFER, 0, buf, -1, 64);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    // The transform-feedback indexed target is independent of the uniform-buffer one.
    gles::glBindBufferBase(GL_TRANSFORM_FEEDBACK_BUFFER, 1, buf);
    assert_eq!(gles::indexed_buffer_binding(GL_TRANSFORM_FEEDBACK_BUFFER, 1), Some((buf, 0, 0)));
    assert_eq!(gles::indexed_buffer_binding(GL_UNIFORM_BUFFER, 1), None);
}
