use super::*;

#[test]
fn sampler_gen_bind_and_parameter_round_trip() {
    let mut c = ctx();
    let s = c.samplers.gen();
    assert_ne!(s, 0, "glGenSamplers must hand out a non-zero name");
    // A merely-reserved name is not yet a sampler OBJECT (lazy instantiation, matching GL).
    assert!(!c.samplers.contains(s));

    // Bind it to a unit — this instantiates the object; the binding round-trips.
    es3::bind_sampler(&mut c, 3, s);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(
        c.samplers.contains(s),
        "a bound sampler is a created object"
    );
    assert_eq!(c.samplers.binding(3), s);

    // A parameter set round-trips through the getter.
    es3::sampler_parameter(
        &mut c,
        s,
        GL_TEXTURE_MIN_FILTER,
        GL_NEAREST as i32,
        GL_NEAREST as f32,
    );
    es3::sampler_parameter(
        &mut c,
        s,
        GL_TEXTURE_WRAP_S,
        GL_CLAMP_TO_EDGE as i32,
        GL_CLAMP_TO_EDGE as f32,
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        es3::get_sampler_parameter(&mut c, s, GL_TEXTURE_MIN_FILTER),
        Some(GL_NEAREST as f32)
    );
    assert_eq!(
        es3::get_sampler_parameter(&mut c, s, GL_TEXTURE_WRAP_S),
        Some(GL_CLAMP_TO_EDGE as f32)
    );

    // Defaults on an untouched parameter (ES 3.0 sampler table): MAG filter defaults to LINEAR.
    assert_eq!(
        es3::get_sampler_parameter(&mut c, s, GL_TEXTURE_MAG_FILTER),
        Some(GL_LINEAR as f32)
    );
}

#[test]
fn sampler_rejects_bad_enum_and_unknown_name() {
    let mut c = ctx();
    let s = c.samplers.gen();

    // An out-of-range enum value raises GL_INVALID_ENUM and leaves the object untouched.
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_MIN_FILTER, 0xDEAD, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    // A sampler name never handed out by glGenSamplers is GL_INVALID_OPERATION.
    es3::sampler_parameter(&mut c, 9999, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // Deleting a bound sampler unbinds it from its unit.
    es3::bind_sampler(&mut c, 1, s);
    c.samplers.delete(s);
    assert_eq!(c.samplers.binding(1), 0);
    assert!(!c.samplers.contains(s));
}
