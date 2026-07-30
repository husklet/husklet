use super::*;

#[test]
fn gl_buffer_upload_contents_marshal_through_map() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_buffer = f!(sh.gles, "glBindBuffer", extern "C" fn(u32, u32));
    let gl_buffer_data = f!(
        sh.gles,
        "glBufferData",
        extern "C" fn(u32, isize, *const c_void, u32)
    );
    let gl_buffer_sub_data = f!(
        sh.gles,
        "glBufferSubData",
        extern "C" fn(u32, isize, isize, *const c_void)
    );
    let gl_map_buffer_range = f!(
        sh.gles,
        "glMapBufferRange",
        extern "C" fn(u32, isize, isize, u32) -> *mut c_void
    );
    let gl_unmap_buffer = f!(sh.gles, "glUnmapBuffer", extern "C" fn(u32) -> u8);
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    let mut buf: u32 = 0;
    gl_gen_buffers(1, &mut buf);
    assert!(buf != 0);
    gl_bind_buffer(GL_ARRAY_BUFFER, buf);

    // glBufferData: a 64-byte array-in upload (distinct byte pattern so a wrong offset/size shows up).
    let src: Vec<u8> = (0u16..64)
        .map(|i| (i.wrapping_mul(7) & 0xFF) as u8)
        .collect();
    gl_buffer_data(
        GL_ARRAY_BUFFER,
        src.len() as isize,
        src.as_ptr() as *const c_void,
        GL_STATIC_DRAW,
    );
    assert_eq!(gl_get_error(), GL_NO_ERROR);

    // glBufferSubData: overwrite bytes [16,24) with a second array-in payload.
    let patch: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44];
    gl_buffer_sub_data(
        GL_ARRAY_BUFFER,
        16,
        patch.len() as isize,
        patch.as_ptr() as *const c_void,
    );
    assert_eq!(gl_get_error(), GL_NO_ERROR);

    // glMapBufferRange returns a pointer INTO the buffer's host storage; read the whole 64 bytes back.
    let p = gl_map_buffer_range(GL_ARRAY_BUFFER, 0, 64, GL_MAP_READ_BIT);
    assert!(
        !p.is_null(),
        "glMapBufferRange returns a non-null host pointer"
    );
    let mapped = unsafe { std::slice::from_raw_parts(p as *const u8, 64) };
    let mut expect = src.clone();
    expect[16..24].copy_from_slice(&patch);
    assert_eq!(
        mapped,
        &expect[..],
        "glBufferData + glBufferSubData contents marshalled byte-for-byte"
    );
    assert_ne!(gl_unmap_buffer(GL_ARRAY_BUFFER), 0);

    // Error paths on the SAME entry point (out-of-range map + unbound target).
    let _ = gl_get_error();
    assert!(
        gl_map_buffer_range(GL_ARRAY_BUFFER, 0, 128, GL_MAP_READ_BIT).is_null(),
        "over-size map fails"
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "offset+length > size → GL_INVALID_VALUE"
    );
    gl_bind_buffer(GL_ARRAY_BUFFER, 0);
    let _ = gl_get_error();
    assert!(
        gl_map_buffer_range(GL_ARRAY_BUFFER, 0, 8, GL_MAP_READ_BIT).is_null(),
        "no bound buffer → null"
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_OPERATION,
        "mapping with no bound buffer → GL_INVALID_OPERATION"
    );
}

// ==================================================================================================
// 7) Array-IN draw path — glVertexAttribPointer + glDrawArrays / glDrawElements over VBO-backed AND
//    client-side vertex/index arrays (the shim reads guest memory through the marshalled pointer), plus
//    their GL_INVALID_* error paths. Recording succeeds without a live executor (IR lowers at swap).
// ==================================================================================================
