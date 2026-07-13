//! GL/EGL enum constants used by the ported entry points. Values are the canonical Khronos numbers
//! (also `#define`d identically in `gl_shim.c`); kept in one place so the state machine and the entry
//! points reference names, not magic hex.
#![allow(dead_code)]

// glGetError / booleans
pub const GL_NO_ERROR: u32 = 0;
pub const GL_INVALID_ENUM: u32 = 0x0500;
pub const GL_INVALID_VALUE: u32 = 0x0501;
pub const GL_INVALID_OPERATION: u32 = 0x0502;
pub const GL_INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;
pub const GL_OUT_OF_MEMORY: u32 = 0x0505;
/// Returned by index queries (glGetUniformBlockIndex / glGetProgramResourceIndex) for "not found".
pub const GL_INVALID_INDEX: u32 = 0xFFFF_FFFF;
pub const GL_FALSE: u8 = 0;
pub const GL_TRUE: u8 = 1;

// shader / program object queries
pub const GL_VERTEX_SHADER: u32 = 0x8B31;
pub const GL_FRAGMENT_SHADER: u32 = 0x8B30;
pub const GL_COMPILE_STATUS: u32 = 0x8B81;
pub const GL_LINK_STATUS: u32 = 0x8B82;
pub const GL_INFO_LOG_LENGTH: u32 = 0x8B84;
pub const GL_SHADER_SOURCE_LENGTH: u32 = 0x8B88;
pub const GL_DELETE_STATUS: u32 = 0x8B80;
pub const GL_ATTACHED_SHADERS: u32 = 0x8B85;

// buffer targets
pub const GL_ARRAY_BUFFER: u32 = 0x8892;
pub const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
// glGetBufferParameteriv pnames
pub const GL_BUFFER_SIZE: u32 = 0x8764;
pub const GL_BUFFER_USAGE: u32 = 0x8765;

// data types
pub const GL_BYTE: u32 = 0x1400;
pub const GL_UNSIGNED_BYTE: u32 = 0x1401;
pub const GL_SHORT: u32 = 0x1402;
pub const GL_UNSIGNED_SHORT: u32 = 0x1403;
pub const GL_INT: u32 = 0x1404;
pub const GL_UNSIGNED_INT: u32 = 0x1405;
pub const GL_FLOAT: u32 = 0x1406;

// primitive topology + clear mask
pub const GL_TRIANGLES: u32 = 0x0004;
pub const GL_TRIANGLE_STRIP: u32 = 0x0005;
pub const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
pub const GL_DEPTH_BUFFER_BIT: u32 = 0x0100;

// caps toggled by glEnable/glDisable/glIsEnabled
pub const GL_DEPTH_TEST: u32 = 0x0B71;
pub const GL_BLEND: u32 = 0x0BE2;
pub const GL_CULL_FACE: u32 = 0x0B44;
pub const GL_SCISSOR_TEST: u32 = 0x0C11;

// texture
pub const GL_TEXTURE_2D: u32 = 0x0DE1;
pub const GL_TEXTURE_3D: u32 = 0x806F;
pub const GL_TEXTURE_2D_ARRAY: u32 = 0x8C1A;
pub const GL_TEXTURE0: u32 = 0x84C0;
pub const GL_RGBA: u32 = 0x1908;
pub const GL_RGB: u32 = 0x1907;
pub const GL_RED: u32 = 0x1903;
pub const GL_ALPHA: u32 = 0x1906;
pub const GL_LUMINANCE: u32 = 0x1909;
pub const GL_BGRA_EXT: u32 = 0x80E1;
pub const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
pub const GL_TEXTURE_WRAP_S: u32 = 0x2802;
pub const GL_TEXTURE_WRAP_T: u32 = 0x2803;
pub const GL_NEAREST: u32 = 0x2600;
pub const GL_LINEAR: u32 = 0x2601;
pub const GL_NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
pub const GL_LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
pub const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
pub const GL_LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
pub const GL_REPEAT: u32 = 0x2901;
pub const GL_CLAMP_TO_EDGE: u32 = 0x812F;
pub const GL_MIRRORED_REPEAT: u32 = 0x8370;

// glPixelStorei pnames
pub const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
pub const GL_UNPACK_ROW_LENGTH: u32 = 0x0CF2;
pub const GL_UNPACK_SKIP_ROWS: u32 = 0x0CF3;
pub const GL_UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
pub const GL_PACK_ALIGNMENT: u32 = 0x0D05;
pub const GL_PACK_ROW_LENGTH: u32 = 0x0D02;
pub const GL_PACK_SKIP_ROWS: u32 = 0x0D03;
pub const GL_PACK_SKIP_PIXELS: u32 = 0x0D04;
pub const GL_PIXEL_PACK_BUFFER: u32 = 0x88EB;
pub const GL_PIXEL_PACK_BUFFER_BINDING: u32 = 0x88ED;

// blend factors
pub const GL_ZERO: u32 = 0;
pub const GL_ONE: u32 = 1;
pub const GL_SRC_COLOR: u32 = 0x0300;
pub const GL_ONE_MINUS_SRC_COLOR: u32 = 0x0301;
pub const GL_SRC_ALPHA: u32 = 0x0302;
pub const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
pub const GL_DST_ALPHA: u32 = 0x0304;
pub const GL_ONE_MINUS_DST_ALPHA: u32 = 0x0305;
pub const GL_DST_COLOR: u32 = 0x0306;
pub const GL_ONE_MINUS_DST_COLOR: u32 = 0x0307;
pub const GL_SRC_ALPHA_SATURATE: u32 = 0x0308;
// blend equations
pub const GL_FUNC_ADD: u32 = 0x8006;
pub const GL_FUNC_SUBTRACT: u32 = 0x800A;
pub const GL_FUNC_REVERSE_SUBTRACT: u32 = 0x800B;
pub const GL_MIN: u32 = 0x8007;
pub const GL_MAX: u32 = 0x8008;

// glGetIntegerv pnames (only the ones gl_shim.c answers non-trivially)
pub const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;
pub const GL_MAX_CUBE_MAP_TEXTURE_SIZE: u32 = 0x851C;
pub const GL_MAX_RENDERBUFFER_SIZE: u32 = 0x84E8;
pub const GL_MAX_VERTEX_ATTRIBS: u32 = 0x8869;
pub const GL_MAX_TEXTURE_IMAGE_UNITS: u32 = 0x8872;
pub const GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS: u32 = 0x8B4D;
pub const GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS: u32 = 0x8B4C;
pub const GL_MAX_FRAGMENT_UNIFORM_VECTORS: u32 = 0x8DFD;
pub const GL_MAX_VERTEX_UNIFORM_VECTORS: u32 = 0x8DFB;
pub const GL_MAX_VARYING_VECTORS: u32 = 0x8DFC;
pub const GL_NUM_COMPRESSED_TEXTURE_FORMATS: u32 = 0x86A2;
pub const GL_SAMPLES: u32 = 0x80A9;
pub const GL_CURRENT_PROGRAM: u32 = 0x8B8D;
pub const GL_ACTIVE_TEXTURE: u32 = 0x84E0;
pub const GL_ARRAY_BUFFER_BINDING: u32 = 0x8894;
pub const GL_ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
pub const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
pub const GL_RENDERBUFFER_BINDING: u32 = 0x8CA7;
pub const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
pub const GL_READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
pub const GL_MAX_SAMPLES_ES3: u32 = 0x8D57;
pub const GL_DEPTH_BITS: u32 = 0x0D56;
pub const GL_STENCIL_BITS: u32 = 0x0D57;
pub const GL_RED_BITS: u32 = 0x0D52;
pub const GL_MAX_VIEWPORT_DIMS: u32 = 0x0D3A;
pub const GL_VIEWPORT: u32 = 0x0BA2;
pub const GL_SCISSOR_BOX: u32 = 0x0C10;
pub const GL_MAJOR_VERSION: u32 = 0x821B;
pub const GL_MINOR_VERSION: u32 = 0x821C;
pub const GL_NUM_EXTENSIONS: u32 = 0x821D;
// ES3 sync objects + internalformat queries (glGetSynciv / glClientWaitSync / glGetInternalformativ)
pub const GL_SYNC_STATUS: u32 = 0x9114;
pub const GL_SIGNALED: i32 = 0x9119;
pub const GL_ALREADY_SIGNALED: u32 = 0x911A;
pub const GL_TIMEOUT_EXPIRED: u32 = 0x911B;
pub const GL_CONDITION_SATISFIED: u32 = 0x911C;
pub const GL_WAIT_FAILED: u32 = 0x911D;
pub const GL_SYNC_GPU_COMMANDS_COMPLETE: u32 = 0x9117;
pub const GL_UNSIGNALED: i32 = 0x9118;
pub const GL_SYNC_FLUSH_COMMANDS_BIT: u32 = 0x0001;
pub const GL_TIMEOUT_IGNORED: u64 = u64::MAX;
pub const GL_NUM_SAMPLE_COUNTS: u32 = 0x9380;

// framebuffer / renderbuffer objects
pub const GL_FRAMEBUFFER: u32 = 0x8D40;
pub const GL_RENDERBUFFER: u32 = 0x8D41;
pub const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
pub const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
pub const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
pub const GL_DEPTH_ATTACHMENT: u32 = 0x8D00;
pub const GL_STENCIL_ATTACHMENT: u32 = 0x8D20;
pub const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
pub const GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT: u32 = 0x8CD6;
pub const GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT: u32 = 0x8CD7;
pub const GL_FRAMEBUFFER_DEFAULT: i32 = 0x8218;
pub const GL_TEXTURE_OBJ: i32 = 0x1702; // GL_TEXTURE (attachment object type)
pub const GL_COLOR: u32 = 0x1800; // glClearBufferfv buffer
// glGetFramebufferAttachmentParameteriv pnames
pub const GL_FB_ATTACHMENT_OBJECT_TYPE: u32 = 0x8CD0;
pub const GL_FB_ATTACHMENT_OBJECT_NAME: u32 = 0x8CD1;
pub const GL_FB_ATTACHMENT_TEXTURE_LEVEL: u32 = 0x8CD2;
pub const GL_FB_ATTACHMENT_TEXTURE_CUBE_MAP_FACE: u32 = 0x8CD3;
pub const GL_FB_ATTACHMENT_TEXTURE_LAYER: u32 = 0x8CD4;
pub const GL_FB_ATTACHMENT_RED_SIZE: u32 = 0x8212;
pub const GL_FB_ATTACHMENT_GREEN_SIZE: u32 = 0x8213;
pub const GL_FB_ATTACHMENT_BLUE_SIZE: u32 = 0x8214;
pub const GL_FB_ATTACHMENT_ALPHA_SIZE: u32 = 0x8215;
pub const GL_FB_ATTACHMENT_DEPTH_SIZE: u32 = 0x8216;
pub const GL_FB_ATTACHMENT_STENCIL_SIZE: u32 = 0x8217;
// glGetRenderbufferParameteriv pnames
pub const GL_RB_WIDTH: u32 = 0x8D42;
pub const GL_RB_HEIGHT: u32 = 0x8D43;
pub const GL_RB_INTERNAL_FORMAT: u32 = 0x8D44;
pub const GL_RB_RED_SIZE: u32 = 0x8D50;
pub const GL_RB_GREEN_SIZE: u32 = 0x8D51;
pub const GL_RB_BLUE_SIZE: u32 = 0x8D52;
pub const GL_RB_ALPHA_SIZE: u32 = 0x8D53;
pub const GL_RB_DEPTH_SIZE: u32 = 0x8D54;
pub const GL_RB_STENCIL_SIZE: u32 = 0x8D55;
pub const GL_RB_SAMPLES: u32 = 0x8CAB;

// ---- EGL ----
pub const EGL_FALSE: u32 = 0;
pub const EGL_TRUE: u32 = 1;
pub const EGL_NONE: i32 = 0x3038;
pub const EGL_DONT_CARE: i32 = -1;
pub const EGL_CONTEXT_LOST: i32 = 0x300E;
pub const EGL_SUCCESS: i32 = 0x3000;
pub const EGL_NOT_INITIALIZED: i32 = 0x3001;
pub const EGL_BAD_ACCESS: i32 = 0x3002;
pub const EGL_BAD_ALLOC: i32 = 0x3003;
pub const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
pub const EGL_BAD_CONFIG: i32 = 0x3005;
pub const EGL_BAD_CONTEXT: i32 = 0x3006;
pub const EGL_BAD_CURRENT_SURFACE: i32 = 0x3007;
pub const EGL_BAD_DISPLAY: i32 = 0x3008;
pub const EGL_BAD_MATCH: i32 = 0x3009;
pub const EGL_BAD_NATIVE_PIXMAP: i32 = 0x300A;
pub const EGL_BAD_NATIVE_WINDOW: i32 = 0x300B;
pub const EGL_BAD_PARAMETER: i32 = 0x300C;
pub const EGL_BAD_SURFACE: i32 = 0x300D;
pub const EGL_WIDTH: i32 = 0x3057;
pub const EGL_HEIGHT: i32 = 0x3056;

pub const EGL_BUFFER_SIZE: i32 = 0x3020;
pub const EGL_ALPHA_SIZE: i32 = 0x3021;
pub const EGL_BLUE_SIZE: i32 = 0x3022;
pub const EGL_GREEN_SIZE: i32 = 0x3023;
pub const EGL_RED_SIZE: i32 = 0x3024;
pub const EGL_DEPTH_SIZE: i32 = 0x3025;
pub const EGL_STENCIL_SIZE: i32 = 0x3026;
pub const EGL_CONFIG_CAVEAT: i32 = 0x3027;
pub const EGL_CONFIG_ID: i32 = 0x3028;
pub const EGL_LEVEL: i32 = 0x3029;
pub const EGL_MAX_PBUFFER_HEIGHT: i32 = 0x302A;
pub const EGL_MAX_PBUFFER_PIXELS: i32 = 0x302B;
pub const EGL_MAX_PBUFFER_WIDTH: i32 = 0x302C;
pub const EGL_NATIVE_RENDERABLE: i32 = 0x302D;
pub const EGL_NATIVE_VISUAL_ID: i32 = 0x302E;
pub const EGL_NATIVE_VISUAL_TYPE: i32 = 0x302F;
pub const EGL_SAMPLES: i32 = 0x3031;
pub const EGL_SAMPLE_BUFFERS: i32 = 0x3032;
pub const EGL_SURFACE_TYPE: i32 = 0x3033;
pub const EGL_TRANSPARENT_TYPE: i32 = 0x3034;
pub const EGL_BIND_TO_TEXTURE_RGB: i32 = 0x3039;
pub const EGL_BIND_TO_TEXTURE_RGBA: i32 = 0x303A;
pub const EGL_MIN_SWAP_INTERVAL: i32 = 0x303B;
pub const EGL_MAX_SWAP_INTERVAL: i32 = 0x303C;
pub const EGL_LUMINANCE_SIZE: i32 = 0x303D;
pub const EGL_ALPHA_MASK_SIZE: i32 = 0x303E;
pub const EGL_COLOR_BUFFER_TYPE: i32 = 0x303F;
pub const EGL_RENDERABLE_TYPE: i32 = 0x3040;
pub const EGL_CONFORMANT: i32 = 0x3042;
pub const EGL_RGB_BUFFER: i32 = 0x308E;
pub const EGL_PBUFFER_BIT: i32 = 0x0001;
pub const EGL_WINDOW_BIT: i32 = 0x0004;
pub const EGL_OPENGL_ES_BIT: i32 = 0x0001;
pub const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
pub const EGL_OPENGL_ES3_BIT_KHR: i32 = 0x0040;

pub const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098; // == EGL_CONTEXT_MAJOR_VERSION_KHR
pub const EGL_CONTEXT_MINOR_VERSION_KHR: i32 = 0x30FB;
pub const EGL_CONTEXT_CLIENT_TYPE: i32 = 0x3097;
pub const EGL_OPENGL_ES_API: u32 = 0x30A0;

/// dma-buf DRM format the config advertises as its native visual (matches gl_shim.c).
pub const DRM_FMT_XRGB8888: i32 = 0x3432_5258;
