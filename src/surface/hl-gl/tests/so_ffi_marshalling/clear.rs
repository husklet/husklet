use super::*;

#[test]
fn clear_buffer_family_validates_selectors_and_null_values() {
    let _guard = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(shared) = load() else { return };
    let clear_fv = f!(
        shared.gles,
        "glClearBufferfv",
        extern "C" fn(u32, i32, *const f32)
    );
    let clear_iv = f!(
        shared.gles,
        "glClearBufferiv",
        extern "C" fn(u32, i32, *const i32)
    );
    let clear_uiv = f!(
        shared.gles,
        "glClearBufferuiv",
        extern "C" fn(u32, i32, *const u32)
    );
    let clear_fi = f!(
        shared.gles,
        "glClearBufferfi",
        extern "C" fn(u32, i32, f32, i32)
    );
    let get_error = f!(shared.gles, "glGetError", extern "C" fn() -> u32);
    let color = [0.25, 0.5, 0.75, 1.0];
    let signed = [-2, -1, 0, 1];
    let unsigned = [0, 1, u32::MAX - 1, u32::MAX];

    let _ = get_error();
    clear_fv(GL_COLOR, 0, color.as_ptr());
    assert_eq!(get_error(), GL_NO_ERROR);
    clear_fv(GL_DEPTH, 0, color.as_ptr());
    assert_eq!(get_error(), GL_NO_ERROR);
    clear_iv(GL_STENCIL, 0, signed.as_ptr());
    assert_eq!(get_error(), GL_NO_ERROR);
    clear_iv(GL_COLOR, 0, signed.as_ptr());
    assert_eq!(get_error(), GL_NO_ERROR);
    clear_uiv(GL_COLOR, 0, unsigned.as_ptr());
    assert_eq!(get_error(), GL_NO_ERROR);
    clear_fi(GL_DEPTH_STENCIL, 0, 0.5, 3);
    assert_eq!(get_error(), GL_NO_ERROR);

    clear_fv(0xDEAD, 0, color.as_ptr());
    assert_eq!(get_error(), GL_INVALID_ENUM);
    clear_uiv(GL_DEPTH, 0, unsigned.as_ptr());
    assert_eq!(get_error(), GL_INVALID_ENUM);
    clear_fi(GL_COLOR, 0, 0.5, 3);
    assert_eq!(get_error(), GL_INVALID_ENUM);

    clear_fv(GL_COLOR, 4, color.as_ptr());
    assert_eq!(get_error(), GL_INVALID_VALUE);
    clear_fv(GL_DEPTH, 1, color.as_ptr());
    assert_eq!(get_error(), GL_INVALID_VALUE);
    clear_fi(GL_DEPTH_STENCIL, 1, 0.5, 3);
    assert_eq!(get_error(), GL_INVALID_VALUE);

    clear_fv(GL_COLOR, 0, core::ptr::null());
    assert_eq!(get_error(), GL_INVALID_VALUE);
    clear_iv(GL_STENCIL, 0, core::ptr::null());
    assert_eq!(get_error(), GL_INVALID_VALUE);
}
