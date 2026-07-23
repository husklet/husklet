use super::*;

#[test]
fn gl_object_existence_predicates_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    // gen/delete + Is triples. Each `is_*` returns the GLboolean widened to the codegen's u32 or u8 ABI.
    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_delete_buffers = f!(sh.gles, "glDeleteBuffers", extern "C" fn(i32, *const u32));
    let gl_is_buffer = f!(sh.gles, "glIsBuffer", extern "C" fn(u32) -> u32);
    let gl_gen_textures = f!(sh.gles, "glGenTextures", extern "C" fn(i32, *mut u32));
    let gl_delete_textures = f!(sh.gles, "glDeleteTextures", extern "C" fn(i32, *const u32));
    let gl_is_texture = f!(sh.gles, "glIsTexture", extern "C" fn(u32) -> u32);
    let gl_gen_framebuffers = f!(sh.gles, "glGenFramebuffers", extern "C" fn(i32, *mut u32));
    let gl_delete_framebuffers = f!(
        sh.gles,
        "glDeleteFramebuffers",
        extern "C" fn(i32, *const u32)
    );
    let gl_is_framebuffer = f!(sh.gles, "glIsFramebuffer", extern "C" fn(u32) -> u32);
    let gl_gen_renderbuffers = f!(sh.gles, "glGenRenderbuffers", extern "C" fn(i32, *mut u32));
    let gl_delete_renderbuffers = f!(
        sh.gles,
        "glDeleteRenderbuffers",
        extern "C" fn(i32, *const u32)
    );
    let gl_is_renderbuffer = f!(sh.gles, "glIsRenderbuffer", extern "C" fn(u32) -> u32);
    let gl_gen_vertex_arrays = f!(sh.gles, "glGenVertexArrays", extern "C" fn(i32, *mut u32));
    let gl_delete_vertex_arrays = f!(
        sh.gles,
        "glDeleteVertexArrays",
        extern "C" fn(i32, *const u32)
    );
    let gl_is_vertex_array = f!(sh.gles, "glIsVertexArray", extern "C" fn(u32) -> u32);
    let gl_gen_queries = f!(sh.gles, "glGenQueries", extern "C" fn(i32, *mut u32));
    let gl_delete_queries = f!(sh.gles, "glDeleteQueries", extern "C" fn(i32, *const u32));
    let gl_begin_query = f!(sh.gles, "glBeginQuery", extern "C" fn(u32, u32));
    let gl_end_query = f!(sh.gles, "glEndQuery", extern "C" fn(u32));
    let gl_is_query = f!(sh.gles, "glIsQuery", extern "C" fn(u32) -> u8);
    let gl_gen_samplers = f!(sh.gles, "glGenSamplers", extern "C" fn(i32, *mut u32));
    let gl_delete_samplers = f!(sh.gles, "glDeleteSamplers", extern "C" fn(i32, *const u32));
    let gl_sampler_parameteri = f!(sh.gles, "glSamplerParameteri", extern "C" fn(u32, u32, i32));
    let gl_is_sampler = f!(sh.gles, "glIsSampler", extern "C" fn(u32) -> u8);
    let gl_create_shader = f!(sh.gles, "glCreateShader", extern "C" fn(u32) -> u32);
    let gl_delete_shader = f!(sh.gles, "glDeleteShader", extern "C" fn(u32));
    let gl_is_shader = f!(sh.gles, "glIsShader", extern "C" fn(u32) -> u32);
    let gl_create_program = f!(sh.gles, "glCreateProgram", extern "C" fn() -> u32);
    let gl_delete_program = f!(sh.gles, "glDeleteProgram", extern "C" fn(u32));
    let gl_is_program = f!(sh.gles, "glIsProgram", extern "C" fn(u32) -> u32);
    let gl_is_sync = f!(sh.gles, "glIsSync", extern "C" fn(*mut c_void) -> u8);
    let gl_fence_sync = f!(
        sh.gles,
        "glFenceSync",
        extern "C" fn(u32, u32) -> *mut c_void
    );
    let gl_client_wait_sync = f!(
        sh.gles,
        "glClientWaitSync",
        extern "C" fn(*mut c_void, u32, u64) -> u32
    );
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    // buffer
    let mut b: u32 = 0;
    gl_gen_buffers(1, &mut b);
    assert_eq!(gl_is_buffer(b), GL_TRUE as u32, "generated buffer exists");
    gl_delete_buffers(1, &b);
    assert_eq!(gl_is_buffer(b), GL_FALSE as u32, "deleted buffer is gone");
    assert_eq!(gl_is_buffer(0), GL_FALSE as u32, "name 0 is never a buffer");
    assert_eq!(
        gl_is_buffer(0xDEAD),
        GL_FALSE as u32,
        "a bogus name is not a buffer"
    );

    // texture
    let mut t: u32 = 0;
    gl_gen_textures(1, &mut t);
    assert_eq!(gl_is_texture(t), GL_TRUE as u32);
    gl_delete_textures(1, &t);
    assert_eq!(gl_is_texture(t), GL_FALSE as u32);

    // framebuffer
    let mut fb: u32 = 0;
    gl_gen_framebuffers(1, &mut fb);
    assert_eq!(gl_is_framebuffer(fb), GL_TRUE as u32);
    gl_delete_framebuffers(1, &fb);
    assert_eq!(gl_is_framebuffer(fb), GL_FALSE as u32);

    // renderbuffer
    let mut rb: u32 = 0;
    gl_gen_renderbuffers(1, &mut rb);
    assert_eq!(gl_is_renderbuffer(rb), GL_TRUE as u32);
    gl_delete_renderbuffers(1, &rb);
    assert_eq!(gl_is_renderbuffer(rb), GL_FALSE as u32);

    // vertex array
    let mut va: u32 = 0;
    gl_gen_vertex_arrays(1, &mut va);
    assert_eq!(gl_is_vertex_array(va), GL_TRUE as u32);
    gl_delete_vertex_arrays(1, &va);
    assert_eq!(gl_is_vertex_array(va), GL_FALSE as u32);

    // query (u8 GLboolean ABI). Per the GL spec a gen'd name is NOT yet a query object — glIsQuery is
    // true only once glBeginQuery instantiates it (the shim models this exactly).
    const GL_ANY_SAMPLES_PASSED: u32 = 0x8C2F;
    let mut q: u32 = 0;
    gl_gen_queries(1, &mut q);
    assert_eq!(
        gl_is_query(q),
        GL_FALSE as u8,
        "a merely-reserved query name is not yet a query object"
    );
    gl_begin_query(GL_ANY_SAMPLES_PASSED, q);
    gl_end_query(GL_ANY_SAMPLES_PASSED);
    assert_eq!(
        gl_is_query(q),
        GL_TRUE as u8,
        "after glBeginQuery the name is a live query object"
    );
    gl_delete_queries(1, &q);
    assert_eq!(gl_is_query(q), GL_FALSE as u8);

    // sampler (u8 GLboolean ABI). Same lazy-instantiation model: glIsSampler is true only once the name
    // acquires state (a glSamplerParameteri), not merely on glGenSamplers.
    const GL_TEXTURE_MIN_FILTER_: u32 = 0x2801;
    let mut sm: u32 = 0;
    gl_gen_samplers(1, &mut sm);
    assert_eq!(
        gl_is_sampler(sm),
        GL_FALSE as u8,
        "a merely-reserved sampler name is not yet an object"
    );
    gl_sampler_parameteri(sm, GL_TEXTURE_MIN_FILTER_, GL_NEAREST);
    assert_eq!(
        gl_is_sampler(sm),
        GL_TRUE as u8,
        "after glSamplerParameteri the name is a live sampler"
    );
    gl_delete_samplers(1, &sm);
    assert_eq!(gl_is_sampler(sm), GL_FALSE as u8);

    // shader
    let s = gl_create_shader(GL_VERTEX_SHADER);
    assert_eq!(gl_is_shader(s), GL_TRUE as u32);
    gl_delete_shader(s);
    assert_eq!(gl_is_shader(s), GL_FALSE as u32);

    // program
    let p = gl_create_program();
    assert_eq!(gl_is_program(p), GL_TRUE as u32);
    gl_delete_program(p);
    assert_eq!(gl_is_program(p), GL_FALSE as u32);

    // sync. A glFenceSync needs a live $HL_GPU_EXEC to land the fence, so its token round-trip is
    // env-dependent — when a token IS minted it reads back as a sync and deletes cleanly; when the submit
    // can't complete it honestly returns null (never a faked token). Either way the *mut c_void return +
    // glIsSync pointer→u8 marshalling is exercised.
    let _ = gl_get_error();
    let syn = gl_fence_sync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
    if !syn.is_null() {
        assert_eq!(
            gl_is_sync(syn),
            GL_TRUE as u8,
            "a live fence sync reads back as a sync"
        );
    }
    assert_eq!(
        gl_is_sync(core::ptr::null_mut()),
        GL_FALSE as u8,
        "null sync is not a sync"
    );
    assert_eq!(
        gl_is_sync(0xDEAD_0000usize as *mut c_void),
        GL_FALSE as u8,
        "bogus sync is not a sync"
    );
    // glClientWaitSync on a bogus (unknown) sync is GL_WAIT_FAILED + GL_INVALID_VALUE (u32 return + error).
    let _ = gl_get_error();
    assert_eq!(
        gl_client_wait_sync(0xDEAD_0000usize as *mut c_void, 0, 0),
        GL_WAIT_FAILED,
        "a bogus sync handle waits with GL_WAIT_FAILED"
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "and raises GL_INVALID_VALUE"
    );
}

// ==================================================================================================
// 4) Program reflection: link a program, then assert real reflected uniform/attribute values through
//    glGetProgramiv / glGetShaderiv / glGet{Uniform,Attrib}Location / glGetActive{Uniform,Attrib} /
//    glGetUniform{fv,iv} / glGetAttachedShaders / glGet{Shader,Program}InfoLog.
// ==================================================================================================
