//! `.so`-integration coverage for the ~65 GL/EGL entry points whose ONLY reachable code lives in the
//! shim's C-ABI marshalling (`shim/egl/src/driver.rs`) — the `glGet*` / `glIs*` / `egl*` families that
//! read/write raw guest pointers and hand back C-ABI-widened results. The `hl_gl` lib exhaustive pass
//! (commit 1b2a66f8) exercises the `hl_gl::service::*` semantics in-process, but it CANNOT reach the
//! `extern "C"` bodies: the `data.is_null()` guards, the `*data.add(i) = buf[i]` out-param stores, the
//! `as i64`/`as u8`/`as f32` width conversions, the `*mut c_void` token round-trips, and the `bool → u32`
//! / `bool → u8` `GLboolean` ABI returns. Chrome (ANGLE) hits these through `dlopen` on `libGLESv2.so.2`
//! / `libEGL.so.1` EXACTLY as this test does, so a marshalling bug here is a real Chrome-facing bug.
//!
//! LOADING: `libGLESv2.so.2` carries a `DT_NEEDED libEGL.so.1` (it imports the shared-state accessor
//! `hl_shim_state_ptr` from libEGL — the two objects share ONE process-global `State`). The staged dir has
//! no `RPATH`, and `LD_LIBRARY_PATH` is frozen at process start, so we `dlopen` `libEGL.so.1` FIRST with
//! `RTLD_GLOBAL` (its `DT_SONAME` is `libEGL.so.1`); `libGLESv2.so.2`'s `NEEDED` then binds to the
//! already-loaded object and `hl_shim_state_ptr` resolves from the global scope.
//!
//! SERIALIZATION: `gl*` (libGLESv2) and `egl*` (libEGL) share the process-global `State`, but cargo runs
//! the `#[test]`s in this ONE binary on parallel threads. So every test grabs [`SERIAL`] first — the GL
//! object model is a single shared context, and concurrent drivers would race it.

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::Mutex;

// ---- dynamic-loader FFI ---------------------------------------------------------------------------
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

/// Serialize every test — they all drive the ONE process-global GL `State`.
static SERIAL: Mutex<()> = Mutex::new(());

// ==================================================================================================
// GL / GLES enum values the caller passes (mirror the shim's constants; a C app passes these raw)
// ==================================================================================================
const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x0500;
const GL_INVALID_VALUE: u32 = 0x0501;
const GL_TRUE: i32 = 1;
const GL_FALSE: i32 = 0;

const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;
const GL_EXTENSIONS: u32 = 0x1F03;
const GL_SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;
const GL_NUM_EXTENSIONS: u32 = 0x821D;

const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;
const GL_MAX_VERTEX_ATTRIBS: u32 = 0x8869;
const GL_MAJOR_VERSION: u32 = 0x821B;
const GL_MINOR_VERSION: u32 = 0x821C;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_DEPTH_BITS: u32 = 0x0D56;
const GL_STENCIL_BITS: u32 = 0x0D57;

const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
const GL_DEPTH_CLEAR_VALUE: u32 = 0x0B73;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_DEPTH_WRITEMASK: u32 = 0x0B72;

const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_UNIFORM_BUFFER: u32 = 0x8A11;
const GL_UNIFORM_BUFFER_BINDING: u32 = 0x8A28;
const GL_BUFFER_SIZE: u32 = 0x8764;
const GL_BUFFER_USAGE: u32 = 0x8765;
const GL_STATIC_DRAW: u32 = 0x88E4;

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_NEAREST: i32 = 0x2600;
const GL_LINEAR: i32 = 0x2601;
const GL_RGBA: u32 = 0x1908;
const GL_RGBA8: u32 = 0x8058;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_WIDTH: u32 = 0x1000;
const GL_TEXTURE_HEIGHT: u32 = 0x1001;
const GL_TEXTURE_INTERNAL_FORMAT: u32 = 0x1003;
const GL_COMPRESSED_RGBA8_ETC2_EAC: u32 = 0x9278;

const GL_RENDERBUFFER: u32 = 0x8D41;
const GL_RENDERBUFFER_WIDTH: u32 = 0x8D42;
const GL_RENDERBUFFER_HEIGHT: u32 = 0x8D43;
const GL_RENDERBUFFER_INTERNAL_FORMAT: u32 = 0x8D44;

const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: u32 = 0x8CD0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: u32 = 0x8CD1;
const GL_TEXTURE: i32 = 0x1702;

const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_VALIDATE_STATUS: u32 = 0x8B83;
const GL_SHADER_TYPE: u32 = 0x8B4F;
const GL_SHADER_SOURCE_LENGTH: u32 = 0x8B88;
const GL_ACTIVE_UNIFORMS: u32 = 0x8B86;
const GL_ACTIVE_ATTRIBUTES: u32 = 0x8B89;
const GL_ATTACHED_SHADERS: u32 = 0x8B85;
const GL_INFO_LOG_LENGTH: u32 = 0x8B84;

const GL_FLOAT: u32 = 0x1406;
const GL_FLOAT_VEC2: u32 = 0x8B50;
const GL_FLOAT_VEC3: u32 = 0x8B51;
const GL_FLOAT_VEC4: u32 = 0x8B52;
const GL_SAMPLER_2D: u32 = 0x8B5E;

const GL_SYNC_GPU_COMMANDS_COMPLETE: u32 = 0x9117;
const GL_WAIT_FAILED: u32 = 0x911D;

// ==================================================================================================
// EGL enum values
// ==================================================================================================
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_FALSE: u32 = 0;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_CONTEXT_CLIENT_TYPE: i32 = 0x3097;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_RENDER_BUFFER: i32 = 0x3086;
const EGL_BACK_BUFFER: i32 = 0x3084;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONDITION_SATISFIED: i32 = 0x30F6;
const EGL_SYNC_FENCE: u32 = 0x30F9;

// ==================================================================================================
// loader helpers
// ==================================================================================================
fn stage_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the so-ffi test: {other}"),
    };
    PathBuf::from(home).join(".hl/gl").join(arch)
}

fn dlopen_global(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if h.is_null() {
        let e = unsafe { dlerror() };
        let msg = if e.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(e) }
                .to_string_lossy()
                .into_owned()
        };
        panic!("dlopen {} failed: {msg}", path.display());
    }
    h
}

fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(
        !p.is_null(),
        "symbol {name} not resolvable in the dlopened object"
    );
    p
}

/// Resolve a symbol from a handle and transmute it to the given `extern "C" fn` type.
macro_rules! f {
    ($h:expr, $name:literal, $ty:ty) => {
        unsafe { core::mem::transmute::<*mut c_void, $ty>(sym($h, $name)) }
    };
}

/// The loaded shim: (libGLESv2 handle for `gl*`, libEGL handle for `egl*`). `None` when the shim is not
/// staged (an x86_64 host without the guest std — see build.rs), so the test skips instead of failing.
struct Shim {
    gles: *mut c_void,
    egl: *mut c_void,
}

fn load() -> Option<Shim> {
    let dir = stage_dir();
    let egl_path = dir.join("libEGL.so.1");
    let gles_path = dir.join("libGLESv2.so.2");
    if !egl_path.exists() || !gles_path.exists() {
        eprintln!(
            "staged shim missing under {} — skipping (guest std not installed)",
            dir.display()
        );
        return None;
    }
    // Never touch a live compositor from any surface path.
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    // libEGL FIRST + RTLD_GLOBAL so libGLESv2's `DT_NEEDED libEGL.so.1` binds it and `hl_shim_state_ptr`
    // resolves from the global scope; then the gl* object.
    let egl = dlopen_global(&egl_path);
    let gles = dlopen_global(&gles_path);
    // The GL/EGL error registers live on the ONE process-global `State`. Tests are serialized (SERIAL),
    // but their run ORDER is nondeterministic, so drain both registers to a clean slate before each test
    // asserts on error state (glGetError / eglGetError are read-and-clear).
    let gl_get_error = f!(gles, "glGetError", extern "C" fn() -> u32);
    let egl_get_error = f!(egl, "eglGetError", extern "C" fn() -> i32);
    gl_get_error();
    egl_get_error();
    Some(Shim { gles, egl })
}

fn cstr(p: *const u8) -> String {
    assert!(
        !p.is_null(),
        "GL string getter returned null (an app dereferences it unconditionally)"
    );
    unsafe { std::ffi::CStr::from_ptr(p as *const c_char) }
        .to_string_lossy()
        .into_owned()
}

// ==================================================================================================
// 1) glGetString / glGetStringi / glGetIntegerv / glGetFloatv / glGetBooleanv / glGetInteger64v
// ==================================================================================================
#[test]
fn gl_identity_and_scalar_state_queries_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_get_string = f!(sh.gles, "glGetString", extern "C" fn(u32) -> *const u8);
    let gl_get_stringi = f!(
        sh.gles,
        "glGetStringi",
        extern "C" fn(u32, u32) -> *const u8
    );
    let gl_get_integerv = f!(sh.gles, "glGetIntegerv", extern "C" fn(u32, *mut i32));
    let gl_get_integer64v = f!(sh.gles, "glGetInteger64v", extern "C" fn(u32, *mut i64));
    let gl_get_floatv = f!(sh.gles, "glGetFloatv", extern "C" fn(u32, *mut f32));
    let gl_get_booleanv = f!(sh.gles, "glGetBooleanv", extern "C" fn(u32, *mut u8));
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);
    let gl_clear_color = f!(sh.gles, "glClearColor", extern "C" fn(f32, f32, f32, f32));
    let gl_clear_depthf = f!(sh.gles, "glClearDepthf", extern "C" fn(f32));
    let gl_enable = f!(sh.gles, "glEnable", extern "C" fn(u32));
    let gl_depth_mask = f!(sh.gles, "glDepthMask", extern "C" fn(u8));

    // glGetString: the guest-visible GLES3 identity strings — exact values ANGLE parses.
    assert_eq!(cstr(gl_get_string(GL_VENDOR)), "hl-gl");
    assert_eq!(cstr(gl_get_string(GL_RENDERER)), "hl-gl-metal");
    assert_eq!(cstr(gl_get_string(GL_VERSION)), "OpenGL ES 3.0 hl-gl");
    assert_eq!(
        cstr(gl_get_string(GL_SHADING_LANGUAGE_VERSION)),
        "OpenGL ES GLSL ES 3.00"
    );
    assert_eq!(
        cstr(gl_get_string(GL_EXTENSIONS)),
        "",
        "no non-core extensions are advertised"
    );

    // glGetIntegerv scalar limits — the truthful executor ceiling, never uninitialized garbage.
    let getint = |p: u32| {
        let mut v: i32 = -455_764_240;
        gl_get_integerv(p, &mut v);
        v
    };
    assert_eq!(getint(GL_MAX_TEXTURE_SIZE), 16384);
    assert_eq!(getint(GL_MAX_VERTEX_ATTRIBS), 16);
    assert_eq!(getint(GL_MAJOR_VERSION), 3);
    assert_eq!(getint(GL_MINOR_VERSION), 0);
    assert_eq!(
        getint(GL_NUM_EXTENSIONS),
        0,
        "matches the empty GL_EXTENSIONS list"
    );
    assert_eq!(getint(GL_DEPTH_BITS), 24);
    assert_eq!(getint(GL_STENCIL_BITS), 8);
    assert_eq!(
        getint(0xBEEF),
        0,
        "an unknown pname writes a single 0, never garbage"
    );

    // A multi-slot query (GL_VIEWPORT writes 4 ints). Drive glViewport (4 ints in) then read it back so
    // the round-trip is DETERMINISTIC and self-contained: the default viewport falls back to the surface
    // extent, which is 0x0 until some surface is made current — so asserting a non-zero default made this
    // depend on another (serialized-but-unordered) test having seeded a surface first. Setting the viewport
    // explicitly exercises the same multi-slot out-param marshalling without that ordering dependency.
    let gl_viewport = f!(sh.gles, "glViewport", extern "C" fn(i32, i32, i32, i32));
    gl_viewport(0, 0, 800, 600);
    let mut vp = [-1i32; 4];
    gl_get_integerv(GL_VIEWPORT, vp.as_mut_ptr());
    assert_eq!(
        vp,
        [0, 0, 800, 600],
        "glViewport -> glGetIntegerv(GL_VIEWPORT) round-trips 4 ints"
    );

    // glGetInteger64v: the SAME ceiling widened to i64 (width-conversion marshalling).
    let mut v64: i64 = -1;
    gl_get_integer64v(GL_MAX_TEXTURE_SIZE, &mut v64);
    assert_eq!(v64, 16384);

    // glGetStringi: out-of-range index raises GL_INVALID_VALUE and returns null (never a dangling ptr).
    assert!(
        gl_get_stringi(GL_EXTENSIONS, 0).is_null(),
        "no extension #0 in an empty inventory"
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "OOB glGetStringi index -> GL_INVALID_VALUE"
    );
    assert!(
        gl_get_stringi(0xBEEF, 0).is_null(),
        "a non-GL_EXTENSIONS name is null"
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_ENUM,
        "bad glGetStringi name -> GL_INVALID_ENUM"
    );

    // glGetFloatv: GL_COLOR_CLEAR_VALUE writes the 4 floats glClearColor recorded.
    gl_clear_color(0.25, 0.5, 0.75, 1.0);
    let mut cc = [-1f32; 4];
    gl_get_floatv(GL_COLOR_CLEAR_VALUE, cc.as_mut_ptr());
    assert_eq!(cc, [0.25, 0.5, 0.75, 1.0]);
    gl_clear_depthf(0.5);
    let mut cd = [-1f32; 1];
    gl_get_floatv(GL_DEPTH_CLEAR_VALUE, cd.as_mut_ptr());
    assert_eq!(cd[0], 0.5);

    // glGetBooleanv: the GLboolean (u8) enable state, exact 1/0.
    gl_enable(GL_DEPTH_TEST);
    let mut b: u8 = 0xAB;
    gl_get_booleanv(GL_DEPTH_TEST, &mut b);
    assert_eq!(b, 1, "glEnable(GL_DEPTH_TEST) reads back as GLboolean 1");
    gl_depth_mask(0);
    let mut dw: u8 = 0xAB;
    gl_get_booleanv(GL_DEPTH_WRITEMASK, &mut dw);
    assert_eq!(dw, 0, "glDepthMask(false) reads back as GLboolean 0");

    // Null out-params are ignored without a deref (the guards on every getter).
    gl_get_integerv(GL_MAX_TEXTURE_SIZE, core::ptr::null_mut());
    gl_get_floatv(GL_COLOR_CLEAR_VALUE, core::ptr::null_mut());
    gl_get_booleanv(GL_DEPTH_TEST, core::ptr::null_mut());
    gl_get_integer64v(GL_MAX_TEXTURE_SIZE, core::ptr::null_mut());
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "null-safe getters raise no error"
    );
}

// ==================================================================================================
// 2) Indexed queries: glGetIntegeri_v / glGetInteger64i_v / glGetBooleani_v after a base binding
// ==================================================================================================
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
#[test]
fn gl_program_reflection_marshals_real_values() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_create_shader = f!(sh.gles, "glCreateShader", extern "C" fn(u32) -> u32);
    let gl_shader_source = f!(
        sh.gles,
        "glShaderSource",
        extern "C" fn(u32, i32, *const *const c_char, *const i32)
    );
    let gl_compile_shader = f!(sh.gles, "glCompileShader", extern "C" fn(u32));
    let gl_create_program = f!(sh.gles, "glCreateProgram", extern "C" fn() -> u32);
    let gl_attach_shader = f!(sh.gles, "glAttachShader", extern "C" fn(u32, u32));
    let gl_link_program = f!(sh.gles, "glLinkProgram", extern "C" fn(u32));
    let gl_use_program = f!(sh.gles, "glUseProgram", extern "C" fn(u32));
    let gl_get_shaderiv = f!(sh.gles, "glGetShaderiv", extern "C" fn(u32, u32, *mut i32));
    let gl_get_programiv = f!(sh.gles, "glGetProgramiv", extern "C" fn(u32, u32, *mut i32));
    let gl_get_uniform_location = f!(
        sh.gles,
        "glGetUniformLocation",
        extern "C" fn(u32, *const c_char) -> i32
    );
    let gl_get_attrib_location = f!(
        sh.gles,
        "glGetAttribLocation",
        extern "C" fn(u32, *const c_char) -> i32
    );
    let gl_get_active_uniform = f!(
        sh.gles,
        "glGetActiveUniform",
        extern "C" fn(u32, u32, i32, *mut i32, *mut i32, *mut u32, *mut c_char)
    );
    let gl_get_active_attrib = f!(
        sh.gles,
        "glGetActiveAttrib",
        extern "C" fn(u32, u32, i32, *mut i32, *mut i32, *mut u32, *mut c_char)
    );
    let gl_get_attached_shaders = f!(
        sh.gles,
        "glGetAttachedShaders",
        extern "C" fn(u32, i32, *mut i32, *mut u32)
    );
    let gl_get_program_info_log = f!(
        sh.gles,
        "glGetProgramInfoLog",
        extern "C" fn(u32, i32, *mut i32, *mut c_char)
    );
    let gl_get_shader_info_log = f!(
        sh.gles,
        "glGetShaderInfoLog",
        extern "C" fn(u32, i32, *mut i32, *mut c_char)
    );
    let gl_uniform4f = f!(
        sh.gles,
        "glUniform4f",
        extern "C" fn(i32, f32, f32, f32, f32)
    );
    let gl_uniform1f = f!(sh.gles, "glUniform1f", extern "C" fn(i32, f32));
    let gl_uniform1i = f!(sh.gles, "glUniform1i", extern "C" fn(i32, i32));
    let gl_get_uniformfv = f!(sh.gles, "glGetUniformfv", extern "C" fn(u32, i32, *mut f32));
    let gl_get_uniformiv = f!(sh.gles, "glGetUniformiv", extern "C" fn(u32, i32, *mut i32));
    let gl_get_uniformuiv = f!(
        sh.gles,
        "glGetUniformuiv",
        extern "C" fn(u32, i32, *mut u32)
    );

    // A VS with two attributes (aPos vec2, aColor vec3) and two data uniforms (uTint vec4, uScale float),
    // and an FS with one sampler (uTex). All reflection is enumerated declaration-order, data-first.
    const VS: &str = "attribute vec2 aPos;\nattribute vec3 aColor;\nuniform vec4 uTint;\nuniform float uScale;\nvoid main(){ gl_Position = vec4(aPos*uScale, aColor.x, 1.0) + uTint; }\n";
    const FS: &str = "precision mediump float;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vec2(0.5)); }\n";

    let compile = |kind: u32, src: &str| -> u32 {
        let s = gl_create_shader(kind);
        let c = std::ffi::CString::new(src).unwrap();
        let ptr = c.as_ptr();
        // count=1, length=null => the shim strlen()s the single NUL-terminated string.
        gl_shader_source(s, 1, &ptr, core::ptr::null());
        gl_compile_shader(s);
        // glGetShaderiv marshalling: COMPILE_STATUS true, SHADER_TYPE echoes the kind, SOURCE_LENGTH = len+1.
        let mut cs: i32 = -1;
        gl_get_shaderiv(s, GL_COMPILE_STATUS, &mut cs);
        assert_eq!(cs, GL_TRUE, "shader compiles");
        let mut ty: i32 = -1;
        gl_get_shaderiv(s, GL_SHADER_TYPE, &mut ty);
        assert_eq!(ty as u32, kind, "GL_SHADER_TYPE echoes the created kind");
        let mut sl: i32 = -1;
        gl_get_shaderiv(s, GL_SHADER_SOURCE_LENGTH, &mut sl);
        assert_eq!(
            sl,
            src.len() as i32 + 1,
            "GL_SHADER_SOURCE_LENGTH counts the NUL"
        );
        s
    };
    let vs = compile(GL_VERTEX_SHADER, VS);
    let fs = compile(GL_FRAGMENT_SHADER, FS);
    let prog = gl_create_program();
    gl_attach_shader(prog, vs);
    gl_attach_shader(prog, fs);
    gl_link_program(prog);
    gl_use_program(prog);

    // glGetProgramiv: link status + reflected counts (2 attrs, 3 uniforms = 2 data + 1 sampler, 2 shaders).
    let getprog = |p: u32| {
        let mut v: i32 = -1;
        gl_get_programiv(prog, p, &mut v);
        v
    };
    assert_eq!(getprog(GL_LINK_STATUS), GL_TRUE);
    assert_eq!(getprog(GL_VALIDATE_STATUS), GL_TRUE);
    assert_eq!(getprog(GL_ATTACHED_SHADERS), 2);
    assert_eq!(getprog(GL_ACTIVE_ATTRIBUTES), 2, "aPos + aColor");
    assert_eq!(getprog(GL_ACTIVE_UNIFORMS), 3, "uTint + uScale + uTex");
    assert_eq!(getprog(GL_INFO_LOG_LENGTH), 0);

    // glGetAttribLocation: declaration-order slots.
    let loc = |func: extern "C" fn(u32, *const c_char) -> i32, name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        func(prog, c.as_ptr())
    };
    assert_eq!(loc(gl_get_attrib_location, "aPos"), 0);
    assert_eq!(loc(gl_get_attrib_location, "aColor"), 1);
    assert_eq!(
        loc(gl_get_attrib_location, "nope"),
        -1,
        "unknown attribute -> -1"
    );

    // glGetUniformLocation: data uniforms indexed first (uTint=0, uScale=1); the sampler uses a SEPARATE
    // index space (uTex=0 among samplers — the shim's modeled location convention).
    assert_eq!(loc(gl_get_uniform_location, "uTint"), 0);
    assert_eq!(loc(gl_get_uniform_location, "uScale"), 1);
    assert_eq!(
        loc(gl_get_uniform_location, "uTex"),
        0,
        "sampler location space is separate"
    );
    assert_eq!(loc(gl_get_uniform_location, "nope"), -1);

    // glGetActiveAttrib: name + GL type + size for each attribute (real reflection into 4 out-params).
    let active = |func: extern "C" fn(u32, u32, i32, *mut i32, *mut i32, *mut u32, *mut c_char),
                  index: u32|
     -> (String, u32, i32, i32) {
        let mut namebuf = [0 as c_char; 64];
        let mut len: i32 = -1;
        let mut size: i32 = -1;
        let mut ty: u32 = 0;
        func(
            prog,
            index,
            namebuf.len() as i32,
            &mut len,
            &mut size,
            &mut ty,
            namebuf.as_mut_ptr(),
        );
        let name = unsafe { std::ffi::CStr::from_ptr(namebuf.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        (name, ty, size, len)
    };
    let a0 = active(gl_get_active_attrib, 0);
    assert_eq!((a0.0.as_str(), a0.1, a0.2), ("aPos", GL_FLOAT_VEC2, 1));
    assert_eq!(
        a0.3, 4,
        "GL_ACTIVE_ATTRIB length excludes the NUL (\"aPos\" = 4)"
    );
    let a1 = active(gl_get_active_attrib, 1);
    assert_eq!((a1.0.as_str(), a1.1, a1.2), ("aColor", GL_FLOAT_VEC3, 1));

    // glGetActiveUniform: data uniforms first (uTint vec4, uScale float), then the sampler (uTex).
    let u0 = active(gl_get_active_uniform, 0);
    assert_eq!((u0.0.as_str(), u0.1, u0.2), ("uTint", GL_FLOAT_VEC4, 1));
    let u1 = active(gl_get_active_uniform, 1);
    assert_eq!((u1.0.as_str(), u1.1, u1.2), ("uScale", GL_FLOAT, 1));
    let u2 = active(gl_get_active_uniform, 2);
    assert_eq!((u2.0.as_str(), u2.1, u2.2), ("uTex", GL_SAMPLER_2D, 1));

    // glGetActiveUniform on an out-of-range index raises GL_INVALID_VALUE + empty name (never OOB).
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);
    let _ = gl_get_error();
    let oob = active(gl_get_active_uniform, 99);
    assert_eq!(oob.0, "", "out-of-range active uniform has an empty name");
    assert_eq!(gl_get_error(), GL_INVALID_VALUE);

    // glGetActiveAttrib name truncation: a tiny buffer writes n-1 chars + NUL, length = n-1.
    let mut tiny = [0 as c_char; 3];
    let mut tlen: i32 = -1;
    gl_get_active_attrib(
        prog,
        1,
        tiny.len() as i32,
        &mut tlen,
        &mut 0,
        &mut 0,
        tiny.as_mut_ptr(),
    );
    let tname = unsafe { std::ffi::CStr::from_ptr(tiny.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        tname, "aC",
        "name truncated to bufSize-1 with a NUL terminator"
    );
    assert_eq!(
        tlen, 2,
        "reported length excludes the NUL and matches the truncation"
    );

    // glGetUniformfv / iv / uiv readback of a set data uniform (uScale = 2.5 at location 1).
    gl_uniform1f(1, 2.5);
    let mut fv: f32 = -1.0;
    gl_get_uniformfv(prog, 1, &mut fv);
    assert_eq!(fv, 2.5, "glGetUniformfv reads back the set data uniform");
    // uTint (vec4) at location 0 — read all 4 components back.
    gl_uniform4f(0, 0.1, 0.2, 0.3, 0.4);
    let mut tint = [-1f32; 4];
    gl_get_uniformfv(prog, 0, tint.as_mut_ptr());
    assert_eq!(tint, [0.1, 0.2, 0.3, 0.4]);
    // The integer bit-pattern reinterpretation (glGetUniformiv/uiv share the same byte copy).
    let mut iv: i32 = 0;
    gl_get_uniformiv(prog, 1, &mut iv);
    assert_eq!(
        f32::from_bits(iv as u32),
        2.5,
        "glGetUniformiv copies the same bytes"
    );
    let mut uiv: u32 = 0;
    gl_get_uniformuiv(prog, 1, &mut uiv);
    assert_eq!(f32::from_bits(uiv), 2.5);

    // glGetAttachedShaders: the real vs+fs attachment names.
    let mut names = [0u32; 4];
    let mut cnt: i32 = -1;
    gl_get_attached_shaders(prog, names.len() as i32, &mut cnt, names.as_mut_ptr());
    assert_eq!(cnt, 2, "two shaders attached");
    let got: Vec<u32> = names[..2].to_vec();
    assert!(
        got.contains(&vs) && got.contains(&fs),
        "attached names are vs+fs, got {got:?}"
    );

    // glGet{Program,Shader}InfoLog: a clean link/compile => empty NUL-terminated log, length 0.
    let mut log = [0x7F as c_char; 32];
    let mut loglen: i32 = -1;
    gl_get_program_info_log(prog, log.len() as i32, &mut loglen, log.as_mut_ptr());
    assert_eq!(loglen, 0, "clean link log is empty");
    assert_eq!(log[0], 0, "info log is NUL-terminated");
    gl_get_shader_info_log(vs, log.len() as i32, &mut loglen, log.as_mut_ptr());
    assert_eq!(loglen, 0);

    // A sampler-only program exercises the glGetUniformiv sampler-unit readback path unambiguously
    // (no data uniform shadows location 0).
    let vs2 = compile(
        GL_VERTEX_SHADER,
        "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n",
    );
    let fs2 = compile(GL_FRAGMENT_SHADER, FS);
    let prog2 = gl_create_program();
    gl_attach_shader(prog2, vs2);
    gl_attach_shader(prog2, fs2);
    gl_link_program(prog2);
    gl_use_program(prog2);
    // glUniform1i binds the sampler (declaration index 0) to texture unit 3; glGetUniformiv reads it back.
    gl_uniform1i(0, 3);
    let mut unit: i32 = -1;
    gl_get_uniformiv(prog2, 0, &mut unit);
    assert_eq!(
        unit, 3,
        "glGetUniformiv on a sampler reports its bound texture unit"
    );
}

// ==================================================================================================
// 5) Object state queries: glGetTexParameter* / glGetBufferParameter* / glGetRenderbufferParameteriv /
//    glGetFramebufferAttachmentParameteriv / glGetTexLevelParameteriv — assert real recorded state.
// ==================================================================================================
#[test]
fn gl_object_state_queries_marshal_real_state() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_buffer = f!(sh.gles, "glBindBuffer", extern "C" fn(u32, u32));
    let gl_buffer_data = f!(
        sh.gles,
        "glBufferData",
        extern "C" fn(u32, isize, *const c_void, u32)
    );
    let gl_get_buffer_parameteriv = f!(
        sh.gles,
        "glGetBufferParameteriv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_get_buffer_parameteri64v = f!(
        sh.gles,
        "glGetBufferParameteri64v",
        extern "C" fn(u32, u32, *mut i64)
    );
    let gl_gen_textures = f!(sh.gles, "glGenTextures", extern "C" fn(i32, *mut u32));
    let gl_bind_texture = f!(sh.gles, "glBindTexture", extern "C" fn(u32, u32));
    let gl_tex_parameteri = f!(sh.gles, "glTexParameteri", extern "C" fn(u32, u32, i32));
    let gl_get_tex_parameteriv = f!(
        sh.gles,
        "glGetTexParameteriv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_get_tex_parameterfv = f!(
        sh.gles,
        "glGetTexParameterfv",
        extern "C" fn(u32, u32, *mut f32)
    );
    let gl_tex_image2d = f!(
        sh.gles,
        "glTexImage2D",
        extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void)
    );
    let gl_get_tex_level_parameteriv = f!(
        sh.gles,
        "glGetTexLevelParameteriv",
        extern "C" fn(u32, i32, u32, *mut i32)
    );
    let gl_gen_renderbuffers = f!(sh.gles, "glGenRenderbuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_renderbuffer = f!(sh.gles, "glBindRenderbuffer", extern "C" fn(u32, u32));
    let gl_renderbuffer_storage = f!(
        sh.gles,
        "glRenderbufferStorage",
        extern "C" fn(u32, u32, i32, i32)
    );
    let gl_get_renderbuffer_parameteriv = f!(
        sh.gles,
        "glGetRenderbufferParameteriv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_gen_framebuffers = f!(sh.gles, "glGenFramebuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_framebuffer = f!(sh.gles, "glBindFramebuffer", extern "C" fn(u32, u32));
    let gl_framebuffer_texture2d = f!(
        sh.gles,
        "glFramebufferTexture2D",
        extern "C" fn(u32, u32, u32, u32, i32)
    );
    let gl_get_fb_attachment_parameteriv = f!(
        sh.gles,
        "glGetFramebufferAttachmentParameteriv",
        extern "C" fn(u32, u32, u32, *mut i32)
    );

    // glGetBufferParameteriv: real byte size + usage of the bound buffer.
    let mut buf: u32 = 0;
    gl_gen_buffers(1, &mut buf);
    gl_bind_buffer(GL_ARRAY_BUFFER, buf);
    let bytes = [7u8; 48];
    gl_buffer_data(
        GL_ARRAY_BUFFER,
        bytes.len() as isize,
        bytes.as_ptr() as *const c_void,
        GL_STATIC_DRAW,
    );
    let mut sz: i32 = -1;
    gl_get_buffer_parameteriv(GL_ARRAY_BUFFER, GL_BUFFER_SIZE, &mut sz);
    assert_eq!(sz, 48, "GL_BUFFER_SIZE is the uploaded byte length");
    let mut usage: i32 = -1;
    gl_get_buffer_parameteriv(GL_ARRAY_BUFFER, GL_BUFFER_USAGE, &mut usage);
    assert_eq!(
        usage as u32, GL_STATIC_DRAW,
        "GL_BUFFER_USAGE echoes the stored hint"
    );
    // The 64-bit view (width conversion).
    let mut sz64: i64 = -1;
    gl_get_buffer_parameteri64v(GL_ARRAY_BUFFER, GL_BUFFER_SIZE, &mut sz64);
    assert_eq!(sz64, 48);

    // glGetTexParameteriv / fv: filter state of the bound texture (fv shares the same int, widened).
    let mut tex: u32 = 0;
    gl_gen_textures(1, &mut tex);
    gl_bind_texture(GL_TEXTURE_2D, tex);
    gl_tex_parameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    gl_tex_parameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    let mut minf: i32 = -1;
    gl_get_tex_parameteriv(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, &mut minf);
    assert_eq!(
        minf, GL_NEAREST,
        "GL_TEXTURE_MIN_FILTER reads back what was set"
    );
    let mut magf: f32 = -1.0;
    gl_get_tex_parameterfv(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, &mut magf);
    assert_eq!(
        magf as i32, GL_LINEAR,
        "glGetTexParameterfv widens the enum to f32 exactly"
    );

    // glGetTexLevelParameteriv: level-0 extent + internal format of a uploaded texture.
    gl_tex_image2d(
        GL_TEXTURE_2D,
        0,
        GL_RGBA as i32,
        64,
        32,
        0,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
        core::ptr::null(),
    );
    let mut w: i32 = -1;
    let mut h: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut w);
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut h);
    assert_eq!((w, h), (64, 32), "glTexImage2D extent is reflected");
    let mut ifmt: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_INTERNAL_FORMAT, &mut ifmt);
    assert_eq!(
        ifmt as u32, GL_RGBA8,
        "the model materializes every texture as RGBA8"
    );

    // glGetRenderbufferParameteriv: extent + RGBA8 format of the bound renderbuffer.
    let mut rb: u32 = 0;
    gl_gen_renderbuffers(1, &mut rb);
    gl_bind_renderbuffer(GL_RENDERBUFFER, rb);
    gl_renderbuffer_storage(GL_RENDERBUFFER, GL_RGBA8, 100, 50);
    let mut rw: i32 = -1;
    let mut rh: i32 = -1;
    gl_get_renderbuffer_parameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_WIDTH, &mut rw);
    gl_get_renderbuffer_parameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_HEIGHT, &mut rh);
    assert_eq!((rw, rh), (100, 50));
    let mut rifmt: i32 = -1;
    gl_get_renderbuffer_parameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_INTERNAL_FORMAT, &mut rifmt);
    assert_eq!(rifmt as u32, GL_RGBA8);

    // glGetFramebufferAttachmentParameteriv: a color-attached texture reflects TYPE=GL_TEXTURE + its name.
    let mut fb: u32 = 0;
    gl_gen_framebuffers(1, &mut fb);
    gl_bind_framebuffer(GL_FRAMEBUFFER, fb);
    gl_framebuffer_texture2d(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    let mut atype: i32 = -1;
    gl_get_fb_attachment_parameteriv(
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
        &mut atype,
    );
    assert_eq!(
        atype, GL_TEXTURE,
        "the color attachment's object type is GL_TEXTURE"
    );
    let mut aname: i32 = -1;
    gl_get_fb_attachment_parameteriv(
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
        &mut aname,
    );
    assert_eq!(aname as u32, tex, "and its name is the attached texture");
}

// ==================================================================================================
// 6) Compressed + copy texture uploads: assert the destination extent is allocated (reflected via
//    glGetTexLevelParameteriv) even though no pixels are decoded/copied (honest model behavior).
// ==================================================================================================
#[test]
fn gl_compressed_and_copy_textures_allocate_extent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_textures = f!(sh.gles, "glGenTextures", extern "C" fn(i32, *mut u32));
    let gl_bind_texture = f!(sh.gles, "glBindTexture", extern "C" fn(u32, u32));
    let gl_compressed_tex_image2d = f!(
        sh.gles,
        "glCompressedTexImage2D",
        extern "C" fn(u32, i32, u32, i32, i32, i32, i32, *const c_void)
    );
    let gl_copy_tex_image2d = f!(
        sh.gles,
        "glCopyTexImage2D",
        extern "C" fn(u32, i32, u32, i32, i32, i32, i32, i32)
    );
    let gl_get_tex_level_parameteriv = f!(
        sh.gles,
        "glGetTexLevelParameteriv",
        extern "C" fn(u32, i32, u32, *mut i32)
    );
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    // glCompressedTexImage2D allocates the bound texture's extent (payload ignored — RGBA8 can't decode).
    let mut ct: u32 = 0;
    gl_gen_textures(1, &mut ct);
    gl_bind_texture(GL_TEXTURE_2D, ct);
    let payload = [0u8; 128];
    gl_compressed_tex_image2d(
        GL_TEXTURE_2D,
        0,
        GL_COMPRESSED_RGBA8_ETC2_EAC,
        16,
        16,
        0,
        payload.len() as i32,
        payload.as_ptr() as *const c_void,
    );
    let mut cw: i32 = -1;
    let mut ch: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut cw);
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut ch);
    assert_eq!(
        (cw, ch),
        (16, 16),
        "compressed upload allocated the 16x16 extent"
    );

    // glCopyTexImage2D allocates the destination extent from the read framebuffer region.
    let mut pt: u32 = 0;
    gl_gen_textures(1, &mut pt);
    gl_bind_texture(GL_TEXTURE_2D, pt);
    let _ = gl_get_error();
    gl_copy_tex_image2d(GL_TEXTURE_2D, 0, GL_RGBA, 0, 0, 24, 24, 0);
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "a valid glCopyTexImage2D raises no error"
    );
    let mut pw: i32 = -1;
    let mut ph: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut pw);
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut ph);
    assert_eq!(
        (pw, ph),
        (24, 24),
        "glCopyTexImage2D allocated the 24x24 destination extent"
    );

    // A bad target/border is GL_INVALID_VALUE (the validated marshalling path).
    gl_copy_tex_image2d(
        GL_TEXTURE_2D,
        0,
        GL_RGBA,
        0,
        0,
        8,
        8,
        1, /* bad border */
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "a non-zero border is rejected"
    );
}

// ==================================================================================================
// 7) EGL round-trips: eglCreateContext / eglMakeCurrent / eglQueryContext / eglQuerySurface /
//    eglCreateSync + eglClientWaitSync / eglCreateImage / eglWaitClient / eglSwapInterval + the
//    per-thread current-binding getters. Driven over the surfaceless display the eglinfo path uses.
// ==================================================================================================
#[test]
fn egl_context_surface_sync_roundtrips_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };
    // Deterministic surface geometry for eglQuerySurface.
    std::env::set_var("HL_GL_SURFACE_W", "640");
    std::env::set_var("HL_GL_SURFACE_H", "480");

    let egl_get_proc = f!(
        sh.egl,
        "eglGetProcAddress",
        extern "C" fn(*const c_char) -> *mut c_void
    );
    let egl_initialize = f!(
        sh.egl,
        "eglInitialize",
        extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32
    );
    let egl_create_context = f!(
        sh.egl,
        "eglCreateContext",
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void
    );
    let egl_make_current = f!(
        sh.egl,
        "eglMakeCurrent",
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32
    );
    let egl_get_current_context = f!(
        sh.egl,
        "eglGetCurrentContext",
        extern "C" fn() -> *mut c_void
    );
    let egl_get_current_display = f!(
        sh.egl,
        "eglGetCurrentDisplay",
        extern "C" fn() -> *mut c_void
    );
    let egl_get_current_surface = f!(
        sh.egl,
        "eglGetCurrentSurface",
        extern "C" fn(i32) -> *mut c_void
    );
    let egl_query_context = f!(
        sh.egl,
        "eglQueryContext",
        extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32
    );
    let egl_query_surface = f!(
        sh.egl,
        "eglQuerySurface",
        extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32
    );
    let egl_create_pbuffer = f!(
        sh.egl,
        "eglCreatePbufferSurface",
        extern "C" fn(*mut c_void, *mut c_void, *const i32) -> *mut c_void
    );
    let egl_create_sync = f!(
        sh.egl,
        "eglCreateSync",
        extern "C" fn(*mut c_void, u32, *const isize) -> *mut c_void
    );
    let egl_client_wait_sync = f!(
        sh.egl,
        "eglClientWaitSync",
        extern "C" fn(*mut c_void, *mut c_void, i32, u64) -> i32
    );
    let egl_destroy_sync = f!(
        sh.egl,
        "eglDestroySync",
        extern "C" fn(*mut c_void, *mut c_void) -> u32
    );
    let egl_create_image = f!(
        sh.egl,
        "eglCreateImage",
        extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const isize) -> *mut c_void
    );
    let egl_destroy_image = f!(
        sh.egl,
        "eglDestroyImage",
        extern "C" fn(*mut c_void, *mut c_void) -> u32
    );
    let egl_wait_client = f!(sh.egl, "eglWaitClient", extern "C" fn() -> u32);
    let egl_swap_interval = f!(
        sh.egl,
        "eglSwapInterval",
        extern "C" fn(*mut c_void, i32) -> u32
    );

    // Bring up the surfaceless display (the same path egl_surfaceless_config.rs drives).
    let get_platform_display: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void = unsafe {
        let c = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
        core::mem::transmute(egl_get_proc(c.as_ptr()))
    };
    let dpy = get_platform_display(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!dpy.is_null());
    assert_eq!(egl_initialize(dpy, &mut 0, &mut 0), EGL_TRUE);

    // eglCreateContext hands back a non-null opaque EGLContext token.
    let ctx = egl_create_context(
        dpy,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(
        !ctx.is_null(),
        "eglCreateContext returns a non-null context token"
    );
    let surf = egl_create_pbuffer(dpy, core::ptr::null_mut(), core::ptr::null());
    assert!(
        !surf.is_null(),
        "eglCreatePbufferSurface returns a surface token"
    );

    // eglMakeCurrent binds ctx+surface on THIS thread; the getters report exactly that binding back.
    assert_eq!(egl_make_current(dpy, surf, surf, ctx), EGL_TRUE);
    assert_eq!(
        egl_get_current_context(),
        ctx,
        "eglGetCurrentContext reports the bound context"
    );
    assert_eq!(
        egl_get_current_display(),
        dpy,
        "eglGetCurrentDisplay reports the bound display"
    );
    const EGL_DRAW: i32 = 0x3059;
    const EGL_READ: i32 = 0x305A;
    assert_eq!(
        egl_get_current_surface(EGL_DRAW),
        surf,
        "the draw surface round-trips"
    );
    assert_eq!(
        egl_get_current_surface(EGL_READ),
        surf,
        "the read surface round-trips"
    );

    // eglQueryContext: the fixed GLES3 identity libepoxy classifies on (ANGLE-facing).
    let qctx = |a: i32| {
        let mut v: i32 = -1;
        assert_eq!(egl_query_context(dpy, ctx, a, &mut v), EGL_TRUE);
        v
    };
    assert_eq!(qctx(EGL_CONTEXT_CLIENT_TYPE) as u32, EGL_OPENGL_ES_API);
    assert_eq!(qctx(EGL_CONTEXT_CLIENT_VERSION), 3);
    assert_eq!(qctx(EGL_RENDER_BUFFER), EGL_BACK_BUFFER);
    assert_eq!(
        qctx(0xBEEF),
        0,
        "an unknown attribute reads 0 and still succeeds"
    );
    assert_eq!(
        egl_query_context(dpy, ctx, EGL_CONTEXT_CLIENT_TYPE, core::ptr::null_mut()),
        EGL_FALSE
    );

    // eglQuerySurface: the live surface geometry (640x480 from the env).
    let mut w: i32 = -1;
    let mut h: i32 = -1;
    assert_eq!(egl_query_surface(dpy, surf, EGL_WIDTH, &mut w), EGL_TRUE);
    assert_eq!(egl_query_surface(dpy, surf, EGL_HEIGHT, &mut h), EGL_TRUE);
    assert_eq!(
        (w, h),
        (640, 480),
        "eglQuerySurface reports the live pbuffer extent"
    );
    assert_eq!(
        egl_query_surface(dpy, surf, EGL_WIDTH, core::ptr::null_mut()),
        EGL_FALSE
    );

    // eglCreateSync + eglClientWaitSync: a non-null sync that reports satisfied (deferred model).
    let sync = egl_create_sync(dpy, EGL_SYNC_FENCE, core::ptr::null());
    assert!(
        !sync.is_null(),
        "eglCreateSync returns a non-null EGLSync token"
    );
    assert_eq!(
        egl_client_wait_sync(dpy, sync, 0, 0),
        EGL_CONDITION_SATISFIED,
        "the sync is already satisfied at the deferred model's synchronous completion"
    );
    assert_eq!(egl_destroy_sync(dpy, sync), EGL_TRUE);

    // eglCreateImage: a non-null EGLImage (!= EGL_NO_IMAGE) so an app's null-check passes.
    let img = egl_create_image(
        dpy,
        ctx,
        0x30B9, /* EGL_GL_TEXTURE_2D */
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(
        !img.is_null(),
        "eglCreateImage returns a non-null image token"
    );
    assert_eq!(egl_destroy_image(dpy, img), EGL_TRUE);

    // eglWaitClient / eglSwapInterval succeed (deferred model completes synchronously at swap).
    assert_eq!(egl_wait_client(), EGL_TRUE);
    assert_eq!(egl_swap_interval(dpy, 1), EGL_TRUE);

    // Release the thread binding so we leave no current context dangling for the next serialized test.
    assert_eq!(
        egl_make_current(
            dpy,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert!(
        egl_get_current_context().is_null(),
        "the binding is released"
    );
}
