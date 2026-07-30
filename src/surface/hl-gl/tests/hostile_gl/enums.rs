use super::*;

// ===================================================================================================
// invalid enums → GL_INVALID_ENUM, or a documented safe no-op — never a panic
// ===================================================================================================

/// `glEnable`/`glDisable`/`glBindTexture`/`glTexParameter`/`glBlendFunc` with junk enums must never panic.
/// Unmodeled-but-legal caps (and the untargeted texture target) are honest no-ops (the model tracks only
/// the fixed-function subset it lowers); a following valid call still takes effect.
#[test]
fn junk_enums_to_state_setters_never_panic_and_valid_still_works() {
    let mut c = ctx();
    // A bogus capability is a safe no-op (no error, no state change).
    record::enable(&mut c, 0xDEAD_BEEF);
    record::disable(&mut c, 0x0000_0001);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(!c.blend_enabled());

    // A bogus glBindTexture target + a junk texture name: no panic, no crash.
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, 0xDEAD, 424242);
    // A junk glBlendFunc factor pair is stored verbatim (validated at lowering); no panic.
    record::blend_func(&mut c, 0xDEAD, 0xBEEF);
    record::tex_parameter(&mut c, 0xDEAD, 0xBEEF);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A valid glEnable(GL_BLEND) still takes effect afterwards.
    record::enable(&mut c, GL_BLEND);
    assert!(c.blend_enabled());
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// `glDrawBuffers`/`glReadBuffer` reject a non-color enum with `GL_INVALID_ENUM`, then a valid list works.
#[test]
fn draw_read_buffer_reject_bad_enum_then_valid_works() {
    let mut c = ctx();
    record::draw_buffers(&mut c, &[GL_COLOR_ATTACHMENT0, 0xDEAD]);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    record::read_buffer(&mut c, 0xDEAD);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    record::draw_buffers(&mut c, &[GL_COLOR_ATTACHMENT0, GL_NONE]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draw_buffers(), vec![GL_COLOR_ATTACHMENT0, GL_NONE]);
    record::read_buffer(&mut c, GL_COLOR_ATTACHMENT0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// `glDrawArrays` with a junk `mode` records the draw (topology validated at lowering) without panicking;
/// a following valid draw is recorded too.
#[test]
fn draw_arrays_with_junk_mode_records_without_panicking() {
    let mut c = ctx();
    record::draw_arrays(&mut c, 0xDEAD_BEEF, 0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws().len(), 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.draws().len(), 2);
}
