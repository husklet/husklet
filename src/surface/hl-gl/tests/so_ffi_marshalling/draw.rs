use super::*;

#[test]
fn gl_draw_array_in_paths_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_buffer = f!(sh.gles, "glBindBuffer", extern "C" fn(u32, u32));
    let gl_buffer_data = f!(
        sh.gles,
        "glBufferData",
        extern "C" fn(u32, isize, *const c_void, u32)
    );
    let gl_gen_vertex_arrays = f!(sh.gles, "glGenVertexArrays", extern "C" fn(i32, *mut u32));
    let gl_bind_vertex_array = f!(sh.gles, "glBindVertexArray", extern "C" fn(u32));
    let gl_vertex_attrib_pointer = f!(
        sh.gles,
        "glVertexAttribPointer",
        extern "C" fn(u32, i32, u32, u8, i32, *const c_void)
    );
    let gl_enable_vaa = f!(sh.gles, "glEnableVertexAttribArray", extern "C" fn(u32));
    let gl_draw_arrays = f!(sh.gles, "glDrawArrays", extern "C" fn(u32, i32, i32));
    let gl_draw_elements = f!(
        sh.gles,
        "glDrawElements",
        extern "C" fn(u32, i32, u32, *const c_void)
    );
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    // GLES3 requires a bound VAO to draw.
    let mut vao: u32 = 0;
    gl_gen_vertex_arrays(1, &mut vao);
    gl_bind_vertex_array(vao);

    // ---- VBO-backed vertex array: glVertexAttribPointer's `pointer` is a byte OFFSET (0), not a client
    //      pointer, so the draw records cleanly against the bound buffer.
    let mut vbo: u32 = 0;
    gl_gen_buffers(1, &mut vbo);
    gl_bind_buffer(GL_ARRAY_BUFFER, vbo);
    // 3 vertices × vec2 f32 = 24 bytes.
    let verts: [f32; 6] = [-0.8, -0.8, 0.8, -0.8, 0.0, 0.8];
    gl_buffer_data(
        GL_ARRAY_BUFFER,
        24,
        verts.as_ptr() as *const c_void,
        GL_STATIC_DRAW,
    );
    gl_vertex_attrib_pointer(0, 2, GL_FLOAT, 0, 8, core::ptr::null());
    gl_enable_vaa(0);
    let _ = gl_get_error();
    gl_draw_arrays(GL_TRIANGLES, 0, 3);
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "a valid VBO-backed glDrawArrays records without error"
    );

    // ---- VBO-backed index array: glDrawElements' `indices` is a byte OFFSET into the bound element buffer.
    let mut ebo: u32 = 0;
    gl_gen_buffers(1, &mut ebo);
    gl_bind_buffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
    let idx: [u16; 3] = [0, 1, 2];
    gl_buffer_data(
        GL_ELEMENT_ARRAY_BUFFER,
        6,
        idx.as_ptr() as *const c_void,
        GL_STATIC_DRAW,
    );
    let _ = gl_get_error();
    gl_draw_elements(GL_TRIANGLES, 3, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "a valid VBO-backed glDrawElements records without error"
    );

    // ---- CLIENT-side arrays: no bound buffer, so the shim reads guest memory THROUGH the marshalled
    //      pointer (the ABI path GTK's client-array draws take). A real Rust array backs each pointer.
    gl_bind_buffer(GL_ARRAY_BUFFER, 0);
    gl_bind_buffer(GL_ELEMENT_ARRAY_BUFFER, 0);
    let client_verts: [f32; 6] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
    gl_vertex_attrib_pointer(0, 2, GL_FLOAT, 0, 8, client_verts.as_ptr() as *const c_void);
    gl_enable_vaa(0);
    let client_idx: [u16; 3] = [0, 1, 2];
    let _ = gl_get_error();
    gl_draw_elements(
        GL_TRIANGLES,
        3,
        GL_UNSIGNED_SHORT,
        client_idx.as_ptr() as *const c_void,
    );
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "a client-array glDrawElements reads guest memory + records"
    );
    gl_draw_arrays(GL_TRIANGLES, 0, 3);
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "a client-array glDrawArrays reads guest memory + records"
    );

    // ---- Error-path marshalling on the draw entry points.
    let _ = gl_get_error();
    gl_draw_arrays(GL_TRIANGLES, 0, -1);
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "a negative glDrawArrays count → GL_INVALID_VALUE"
    );
    gl_draw_elements(GL_TRIANGLES, -1, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "a negative glDrawElements count → GL_INVALID_VALUE"
    );
}

// ==================================================================================================
// 8) glReadPixels + glGetVertexAttrib{i,f}v — pointer-out argument + error-path marshalling
// ==================================================================================================
