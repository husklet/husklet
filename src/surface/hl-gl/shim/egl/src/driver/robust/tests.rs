use super::*;

#[test]
fn robust_integer_query_honors_capacity_and_reports_length() {
    let display = initialized_display();
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        1,
        EGL_NONE,
    ];
    let context = eglCreateContext(
        display,
        CONFIG_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context
        ),
        EGL_TRUE
    );
    let mut values = [-1; 4];
    let mut length = -1;
    glGetIntegervRobustANGLE(GL_MAX_VIEWPORT_DIMS, 2, &mut length, values.as_mut_ptr());
    assert_eq!(length, 2);
    assert_eq!(&values[..2], &[query::VIEWPORT_DIM, query::VIEWPORT_DIM]);

    length = -1;
    values = [-1; 4];
    glGetIntegervRobustANGLE(GL_VIEWPORT, 1, &mut length, values.as_mut_ptr());
    assert_eq!(length, 0);
    assert_eq!(values, [-1; 4]);
    assert_eq!(glGetError(), GL_INVALID_OPERATION);
    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
}

#[test]
fn robust_upload_distinguishes_client_memory_from_pbo_offsets() {
    let upload = Upload::new(GL_RGBA, GL_UNSIGNED_BYTE, 2, 1, Default::default()).unwrap();

    assert!(robust_upload_fits(upload, 8, 1, None));
    assert!(!robust_upload_fits(upload, 7, 1, None));
    assert!(robust_upload_fits(upload, 0, 0, None));

    assert!(robust_upload_fits(upload, 0, 0, Some(8)));
    assert!(robust_upload_fits(upload, 0, 4, Some(12)));
    assert!(!robust_upload_fits(upload, 8, 4, Some(12)));
    assert!(!robust_upload_fits(upload, 0, 5, Some(12)));
    assert!(!robust_upload_fits(upload, 0, usize::MAX, Some(12)));
}
