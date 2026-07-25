use super::*;

#[test]
fn query_begin_end_lifecycle_and_result_round_trip() {
    let mut c = ctx();
    let q = c.queries.gen();
    assert_ne!(q, 0);
    assert!(
        !c.queries.contains(q),
        "a reserved name is not yet a query object"
    );

    // Begin makes it the active query for its target.
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, q);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(c.queries.contains(q));
    assert_eq!(
        es3::get_queryiv(&mut c, GL_ANY_SAMPLES_PASSED, GL_CURRENT_QUERY),
        Some(q as i32)
    );

    // A second begin on the same target while active is GL_INVALID_OPERATION.
    let q2 = c.queries.gen();
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, q2);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // End clears the active slot; the result becomes available (deferred model completes synchronously).
    c.end_query(GL_ANY_SAMPLES_PASSED);
    assert_eq!(
        es3::get_queryiv(&mut c, GL_ANY_SAMPLES_PASSED, GL_CURRENT_QUERY),
        Some(0)
    );
    assert_eq!(
        es3::get_query_objectuiv(&mut c, q, GL_QUERY_RESULT_AVAILABLE),
        Some(1)
    );
    // No occlusion executor ⇒ a truthful zero sample count.
    assert_eq!(
        es3::get_query_objectuiv(&mut c, q, GL_QUERY_RESULT),
        Some(0)
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

#[test]
fn query_rejects_bad_target_and_unknown_id() {
    let mut c = ctx();
    // A non-query target is GL_INVALID_ENUM.
    es3::begin_query(&mut c, GL_ARRAY_BUFFER, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // Begin with a name never from glGenQueries is GL_INVALID_OPERATION.
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, 4242);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    // Result of an unknown query is GL_INVALID_OPERATION.
    assert_eq!(
        es3::get_query_objectuiv(&mut c, 4242, GL_QUERY_RESULT),
        None
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

// ---- transform-feedback objects ------------------------------------------------------------------
#[test]
fn delete_query_object_makes_it_no_longer_a_query() {
    let mut c = ctx();
    let q = c.queries.gen();
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, q);
    c.end_query(GL_ANY_SAMPLES_PASSED);
    assert!(c.queries.contains(q), "an instantiated query object");

    c.queries.delete(q);
    assert!(!c.queries.contains(q), "glDeleteQueries drops the object");
    // A deleted name is unknown again: begin on it is GL_INVALID_OPERATION.
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, q);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    // Deleting 0 (and a never-generated name) is a silent no-op.
    c.queries.delete(0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}
