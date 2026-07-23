use super::*;

#[test]
fn gl_indexed_queries_marshal_after_binding() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_buffer_base = f!(sh.gles, "glBindBufferBase", extern "C" fn(u32, u32, u32));
    let gl_get_integeri_v = f!(
        sh.gles,
        "glGetIntegeri_v",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_get_integer64i_v = f!(
        sh.gles,
        "glGetInteger64i_v",
        extern "C" fn(u32, u32, *mut i64)
    );
    let gl_get_booleani_v = f!(sh.gles, "glGetBooleani_v", extern "C" fn(u32, u32, *mut u8));

    let mut ubo: u32 = 0;
    gl_gen_buffers(1, &mut ubo);
    assert!(ubo != 0, "glGenBuffers writes a fresh non-zero name");
    gl_bind_buffer_base(GL_UNIFORM_BUFFER, 2, ubo);

    // glGetIntegeri_v(GL_UNIFORM_BUFFER_BINDING, 2) reports the buffer bound at index 2 (real state).
    let mut idx: i32 = -1;
    gl_get_integeri_v(GL_UNIFORM_BUFFER_BINDING, 2, &mut idx);
    assert_eq!(
        idx as u32, ubo,
        "indexed UBO binding at slot 2 is our buffer"
    );
    // Unbound index 5 reads 0.
    let mut none: i32 = -1;
    gl_get_integeri_v(GL_UNIFORM_BUFFER_BINDING, 5, &mut none);
    assert_eq!(none, 0, "an unbound indexed slot reads 0");

    // The 64-bit view of the same binding (width conversion).
    let mut idx64: i64 = -1;
    gl_get_integer64i_v(GL_UNIFORM_BUFFER_BINDING, 2, &mut idx64);
    assert_eq!(idx64 as u32, ubo);

    // The boolean view: a bound (non-zero) binding is GLboolean 1.
    let mut bidx: u8 = 0xAB;
    gl_get_booleani_v(GL_UNIFORM_BUFFER_BINDING, 2, &mut bidx);
    assert_eq!(
        bidx, 1,
        "a non-zero indexed binding reads back as GLboolean 1"
    );
    let mut bnone: u8 = 0xAB;
    gl_get_booleani_v(GL_UNIFORM_BUFFER_BINDING, 5, &mut bnone);
    assert_eq!(bnone, 0);

    // Null-safe.
    gl_get_integeri_v(GL_UNIFORM_BUFFER_BINDING, 2, core::ptr::null_mut());
    gl_get_integer64i_v(GL_UNIFORM_BUFFER_BINDING, 2, core::ptr::null_mut());
    gl_get_booleani_v(GL_UNIFORM_BUFFER_BINDING, 2, core::ptr::null_mut());
}

// ==================================================================================================
// 3) Object-existence predicates: glIs{Buffer,Texture,Framebuffer,Renderbuffer,VertexArray,Shader,
//    Program,Query,Sampler,Sync} — true after create, false after delete / for a bogus name.
// ==================================================================================================
