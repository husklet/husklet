//! GL/EGL enum constants used by the recording ops and the frame builder. Values are the canonical
//! Khronos numbers (identical to `hl-shim-gl/src/glconst.rs` and `gl_shim.c`'s `#define`s); kept in one
//! place so the state machine and services reference names, not magic hex.
#![allow(dead_code)]

// data types (glVertexAttribPointer / glDrawElements index type)
pub const GL_BYTE: u32 = 0x1400;
pub const GL_UNSIGNED_BYTE: u32 = 0x1401;
pub const GL_SHORT: u32 = 0x1402;
pub const GL_UNSIGNED_SHORT: u32 = 0x1403;
pub const GL_UNSIGNED_INT_2_10_10_10_REV: u32 = 0x8368;
pub const GL_INT_2_10_10_10_REV: u32 = 0x8D9F;
pub const GL_INT: u32 = 0x1404;
pub const GL_UNSIGNED_INT: u32 = 0x1405;
pub const GL_FLOAT: u32 = 0x1406;
pub const GL_HALF_FLOAT: u32 = 0x140B;

// buffer targets
pub const GL_ARRAY_BUFFER: u32 = 0x8892;
pub const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
pub const GL_PIXEL_PACK_BUFFER: u32 = 0x88EB;
pub const GL_PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
pub const GL_UNIFORM_BUFFER: u32 = 0x8A11;
pub const GL_SHADER_STORAGE_BUFFER: u32 = 0x90D2;
pub const GL_ATOMIC_COUNTER_BUFFER: u32 = 0x92C0;
pub const GL_TRANSFORM_FEEDBACK_BUFFER: u32 = 0x8C8E;
pub const GL_DISPATCH_INDIRECT_BUFFER: u32 = 0x90EE;
pub const GL_COPY_READ_BUFFER: u32 = 0x8F36;
pub const GL_COPY_WRITE_BUFFER: u32 = 0x8F37;
pub const GL_DRAW_INDIRECT_BUFFER: u32 = 0x8F3F;

// primitive topology + clear mask
pub const GL_POINTS: u32 = 0x0000;
pub const GL_LINES: u32 = 0x0001;
pub const GL_LINE_STRIP: u32 = 0x0003;
pub const GL_TRIANGLES: u32 = 0x0004;
pub const GL_TRIANGLE_STRIP: u32 = 0x0005;
pub const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
pub const GL_DEPTH_BUFFER_BIT: u32 = 0x0100;
pub const GL_STENCIL_BUFFER_BIT: u32 = 0x0400;

// caps toggled by glEnable/glDisable
pub const GL_DEPTH_TEST: u32 = 0x0B71;
pub const GL_STENCIL_TEST: u32 = 0x0B90;
pub const GL_BLEND: u32 = 0x0BE2;
pub const GL_CULL_FACE: u32 = 0x0B44;
pub const GL_SCISSOR_TEST: u32 = 0x0C11;

// stencil operations (glStencilOp / glStencilOpSeparate). GL_ZERO (0) doubles as the ZERO stencil op.
pub const GL_KEEP: u32 = 0x1E00;
pub const GL_REPLACE: u32 = 0x1E01;
pub const GL_INCR: u32 = 0x1E02;
pub const GL_DECR: u32 = 0x1E03;
pub const GL_INVERT: u32 = 0x150A;
pub const GL_INCR_WRAP: u32 = 0x8507;
pub const GL_DECR_WRAP: u32 = 0x8508;

// renderbuffer depth/stencil internal formats (glRenderbufferStorage)
pub const GL_DEPTH24_STENCIL8: u32 = 0x88F0;
pub const GL_STENCIL_INDEX8: u32 = 0x8D48;

// blend factors (glBlendFunc)
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
pub const GL_SRC1_ALPHA: u32 = 0x8589;
pub const GL_SRC1_COLOR: u32 = 0x88F9;
pub const GL_ONE_MINUS_SRC1_COLOR: u32 = 0x88FA;
pub const GL_ONE_MINUS_SRC1_ALPHA: u32 = 0x88FB;

// blend equations (glBlendEquation)
pub const GL_FUNC_ADD: u32 = 0x8006;
pub const GL_MIN: u32 = 0x8007;
pub const GL_MAX: u32 = 0x8008;
pub const GL_FUNC_SUBTRACT: u32 = 0x800A;
pub const GL_FUNC_REVERSE_SUBTRACT: u32 = 0x800B;

// depth-compare functions (glDepthFunc)
pub const GL_NEVER: u32 = 0x0200;
pub const GL_LESS: u32 = 0x0201;
pub const GL_EQUAL: u32 = 0x0202;
pub const GL_LEQUAL: u32 = 0x0203;
pub const GL_GREATER: u32 = 0x0204;
pub const GL_NOTEQUAL: u32 = 0x0205;
pub const GL_GEQUAL: u32 = 0x0206;
pub const GL_ALWAYS: u32 = 0x0207;

// cull faces + winding (glCullFace / glFrontFace)
pub const GL_FRONT: u32 = 0x0404;
pub const GL_BACK: u32 = 0x0405;
pub const GL_FRONT_AND_BACK: u32 = 0x0408;
pub const GL_CW: u32 = 0x0900;
pub const GL_CCW: u32 = 0x0901;

// shader object kinds
pub const GL_VERTEX_SHADER: u32 = 0x8B31;
pub const GL_FRAGMENT_SHADER: u32 = 0x8B30;
pub const GL_COMPUTE_SHADER: u32 = 0x91B9;

// texture enums
pub const GL_TEXTURE_2D: u32 = 0x0DE1;
pub const GL_TEXTURE0: u32 = 0x84C0;
pub const GL_RGBA: u32 = 0x1908;
pub const GL_RGB: u32 = 0x1907;
pub const GL_BGRA_EXT: u32 = 0x80E1;
pub const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
pub const GL_TEXTURE_WRAP_S: u32 = 0x2802;
pub const GL_TEXTURE_WRAP_T: u32 = 0x2803;
pub const GL_TEXTURE_SWIZZLE_R: u32 = 0x8E42;
pub const GL_TEXTURE_SWIZZLE_G: u32 = 0x8E43;
pub const GL_TEXTURE_SWIZZLE_B: u32 = 0x8E44;
pub const GL_TEXTURE_SWIZZLE_A: u32 = 0x8E45;
pub const GL_TEXTURE_SWIZZLE_RGBA: u32 = 0x8E46;
pub const GL_NEAREST: u32 = 0x2600;
pub const GL_LINEAR: u32 = 0x2601;
pub const GL_NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
pub const GL_LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
pub const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
pub const GL_LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
pub const GL_CLAMP_TO_EDGE: u32 = 0x812F;
pub const GL_REPEAT: u32 = 0x2901;
pub const GL_MIRRORED_REPEAT: u32 = 0x8370;

// boolean literals (glGetBooleanv / *_STATUS)
pub const GL_FALSE: u32 = 0;
pub const GL_TRUE: u32 = 1;

// glGetString name enums (identity strings)
pub const GL_VENDOR: u32 = 0x1F00;
pub const GL_RENDERER: u32 = 0x1F01;
pub const GL_VERSION: u32 = 0x1F02;
pub const GL_EXTENSIONS: u32 = 0x1F03;
pub const GL_SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;

// glGetIntegerv/glGetFloatv/glGetBooleanv pnames — capability limits + bound-object queries.
pub const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;
pub const GL_MAX_CUBE_MAP_TEXTURE_SIZE: u32 = 0x851C;
pub const GL_MAX_3D_TEXTURE_SIZE: u32 = 0x8073;
pub const GL_MAX_ARRAY_TEXTURE_LAYERS: u32 = 0x88FF;
pub const GL_MAX_RENDERBUFFER_SIZE: u32 = 0x84E8;
pub const GL_MAX_VERTEX_ATTRIBS: u32 = 0x8869;
pub const GL_MAX_TEXTURE_IMAGE_UNITS: u32 = 0x8872;
pub const GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS: u32 = 0x8B4D;
pub const GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS: u32 = 0x8B4C;
pub const GL_MAX_FRAGMENT_UNIFORM_COMPONENTS: u32 = 0x8B49;
pub const GL_MAX_VERTEX_UNIFORM_COMPONENTS: u32 = 0x8B4A;
pub const GL_MAX_VARYING_COMPONENTS: u32 = 0x8B4B;
pub const GL_MAX_FRAGMENT_UNIFORM_VECTORS: u32 = 0x8DFD;
pub const GL_MAX_VERTEX_UNIFORM_VECTORS: u32 = 0x8DFB;
pub const GL_MAX_VARYING_VECTORS: u32 = 0x8DFC;
pub const GL_NUM_COMPRESSED_TEXTURE_FORMATS: u32 = 0x86A2;
pub const GL_SAMPLES: u32 = 0x80A9;
pub const GL_MAX_SAMPLES: u32 = 0x8D57;
pub const GL_CURRENT_PROGRAM: u32 = 0x8B8D;
pub const GL_ACTIVE_TEXTURE: u32 = 0x84E0;
pub const GL_ARRAY_BUFFER_BINDING: u32 = 0x8894;
pub const GL_ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
pub const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
pub const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
pub const GL_MAJOR_VERSION: u32 = 0x821B;
pub const GL_MINOR_VERSION: u32 = 0x821C;
pub const GL_NUM_EXTENSIONS: u32 = 0x821D;
pub const GL_REQUESTABLE_EXTENSIONS_ANGLE: u32 = 0x93A8;
pub const GL_NUM_REQUESTABLE_EXTENSIONS_ANGLE: u32 = 0x93A9;
pub const GL_DEPTH_BITS: u32 = 0x0D56;
pub const GL_STENCIL_BITS: u32 = 0x0D57;
pub const GL_RED_BITS: u32 = 0x0D52;
pub const GL_GREEN_BITS: u32 = 0x0D53;
pub const GL_BLUE_BITS: u32 = 0x0D54;
pub const GL_ALPHA_BITS: u32 = 0x0D55;
pub const GL_MAX_VIEWPORT_DIMS: u32 = 0x0D3A;
// GLES3 index/vertex batch ceilings a toolkit (GTK/epoxy) queries before it sizes draw batches.
pub const GL_MAX_ELEMENTS_VERTICES: u32 = 0x80E8;
pub const GL_MAX_ELEMENTS_INDICES: u32 = 0x80E9;
pub const GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS: u32 = 0x8C80;
pub const GL_MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS: u32 = 0x8C8A;
pub const GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS: u32 = 0x8C8B;
pub const GL_MAX_VERTEX_UNIFORM_BLOCKS: u32 = 0x8A2B;
pub const GL_MAX_FRAGMENT_UNIFORM_BLOCKS: u32 = 0x8A2D;
pub const GL_MAX_COMBINED_UNIFORM_BLOCKS: u32 = 0x8A2E;
pub const GL_MAX_UNIFORM_BUFFER_BINDINGS: u32 = 0x8A2F;
pub const GL_MAX_UNIFORM_BLOCK_SIZE: u32 = 0x8A30;
pub const GL_MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS: u32 = 0x8A31;
pub const GL_MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS: u32 = 0x8A33;
pub const GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT: u32 = 0x8A34;
pub const GL_SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT: u32 = 0x90DF;
pub const GL_MIN_PROGRAM_TEXEL_OFFSET: u32 = 0x8904;
pub const GL_MAX_PROGRAM_TEXEL_OFFSET: u32 = 0x8905;
pub const GL_MAX_VERTEX_OUTPUT_COMPONENTS: u32 = 0x9122;
pub const GL_MAX_FRAGMENT_INPUT_COMPONENTS: u32 = 0x9125;
pub const GL_VIEWPORT: u32 = 0x0BA2;
pub const GL_SCISSOR_BOX: u32 = 0x0C10;
pub const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
pub const GL_DEPTH_CLEAR_VALUE: u32 = 0x0B73;
pub const GL_STENCIL_CLEAR_VALUE: u32 = 0x0B91;
pub const GL_LINE_WIDTH: u32 = 0x0B21;
pub const GL_ALIASED_POINT_SIZE_RANGE: u32 = 0x846D;
pub const GL_ALIASED_LINE_WIDTH_RANGE: u32 = 0x846E;
pub const GL_MAX_TEXTURE_LOD_BIAS: u32 = 0x84FD;
pub const GL_COLOR_WRITEMASK: u32 = 0x0C23;
pub const GL_DEPTH_WRITEMASK: u32 = 0x0B72;

// glGetShaderiv / glGetProgramiv pnames.
pub const GL_DELETE_STATUS: u32 = 0x8B80;
pub const GL_COMPILE_STATUS: u32 = 0x8B81;
pub const GL_LINK_STATUS: u32 = 0x8B82;
pub const GL_VALIDATE_STATUS: u32 = 0x8B83;
pub const GL_INFO_LOG_LENGTH: u32 = 0x8B84;
pub const GL_ATTACHED_SHADERS: u32 = 0x8B85;
pub const GL_ACTIVE_UNIFORMS: u32 = 0x8B86;
pub const GL_ACTIVE_UNIFORM_MAX_LENGTH: u32 = 0x8B87;
pub const GL_ACTIVE_ATTRIBUTES: u32 = 0x8B89;
pub const GL_ACTIVE_ATTRIBUTE_MAX_LENGTH: u32 = 0x8B8A;
pub const GL_SHADER_TYPE: u32 = 0x8B4F;
pub const GL_SHADER_SOURCE_LENGTH: u32 = 0x8B88;

// glPixelStorei pnames.
pub const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
pub const GL_PACK_ALIGNMENT: u32 = 0x0D05;
pub const GL_UNPACK_ROW_LENGTH: u32 = 0x0CF2;
pub const GL_UNPACK_SKIP_ROWS: u32 = 0x0CF3;
pub const GL_UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
pub const GL_PACK_ROW_LENGTH: u32 = 0x0D02;
pub const GL_PACK_SKIP_ROWS: u32 = 0x0D03;
pub const GL_PACK_SKIP_PIXELS: u32 = 0x0D04;

// Pixel transfer formats/types.
pub const GL_RED: u32 = 0x1903;
pub const GL_GREEN: u32 = 0x1904;
pub const GL_BLUE: u32 = 0x1905;
pub const GL_ALPHA: u32 = 0x1906;
pub const GL_LUMINANCE: u32 = 0x1909;
pub const GL_LUMINANCE_ALPHA: u32 = 0x190A;
pub const GL_RG: u32 = 0x8227;
pub const GL_UNSIGNED_SHORT_5_6_5: u32 = 0x8363;

// GLSL variable type enums (returned by glGetActiveUniform / glGetActiveAttrib `type`).
pub const GL_FLOAT_VEC2: u32 = 0x8B50;
pub const GL_FLOAT_VEC3: u32 = 0x8B51;
pub const GL_FLOAT_VEC4: u32 = 0x8B52;
pub const GL_INT_VEC2: u32 = 0x8B53;
pub const GL_INT_VEC3: u32 = 0x8B54;
pub const GL_INT_VEC4: u32 = 0x8B55;
pub const GL_BOOL: u32 = 0x8B56;
pub const GL_UNSIGNED_INT_VEC2: u32 = 0x8DC6;
pub const GL_UNSIGNED_INT_VEC3: u32 = 0x8DC7;
pub const GL_UNSIGNED_INT_VEC4: u32 = 0x8DC8;
pub const GL_FLOAT_MAT2: u32 = 0x8B5A;
pub const GL_FLOAT_MAT3: u32 = 0x8B5B;
pub const GL_FLOAT_MAT4: u32 = 0x8B5C;
pub const GL_SAMPLER_2D: u32 = 0x8B5E;
pub const GL_SAMPLER_CUBE: u32 = 0x8B60;

// glGenerateMipmap texture targets.
pub const GL_TEXTURE_CUBE_MAP: u32 = 0x8513;

// GLenum error codes (returned by glGetError; re-declared from result.rs for the model layer).
pub const GL_NO_ERROR: u32 = 0;
pub const GL_INVALID_ENUM: u32 = 0x0500;
pub const GL_INVALID_VALUE: u32 = 0x0501;
pub const GL_INVALID_OPERATION: u32 = 0x0502;
pub const GL_INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;

// MRT draw/read buffer selectors (glDrawBuffers / glReadBuffer). GL_BACK (0x0405) doubles as the
// default-framebuffer draw buffer (declared once, above, as the cull-face enum of the same value).
pub const GL_NONE: u32 = 0;
pub const GL_COLOR_ATTACHMENT1: u32 = 0x8CE1;
pub const GL_MAX_DRAW_BUFFERS: u32 = 0x8824;
pub const GL_MAX_COLOR_ATTACHMENTS: u32 = 0x8CDF;

// sync objects (glFenceSync / glClientWaitSync / glWaitSync / glGetSynciv).
pub const GL_SYNC_GPU_COMMANDS_COMPLETE: u32 = 0x9117;
pub const GL_SYNC_FLUSH_COMMANDS_BIT: u32 = 0x0000_0001;
pub const GL_TIMEOUT_IGNORED: u64 = 0xFFFF_FFFF_FFFF_FFFF;
pub const GL_ALREADY_SIGNALED: u32 = 0x911A;
pub const GL_TIMEOUT_EXPIRED: u32 = 0x911B;
pub const GL_MAP_READ_BIT: u32 = 0x0001;
pub const GL_MAP_WRITE_BIT: u32 = 0x0002;
pub const GL_MAP_INVALIDATE_RANGE_BIT: u32 = 0x0004;
pub const GL_MAP_INVALIDATE_BUFFER_BIT: u32 = 0x0008;
pub const GL_MAP_FLUSH_EXPLICIT_BIT: u32 = 0x0010;
pub const GL_MAP_UNSYNCHRONIZED_BIT: u32 = 0x0020;
pub const GL_CONDITION_SATISFIED: u32 = 0x911C;
pub const GL_WAIT_FAILED: u32 = 0x911D;
pub const GL_OBJECT_TYPE: u32 = 0x9112;
pub const GL_SYNC_CONDITION: u32 = 0x9113;
pub const GL_SYNC_STATUS: u32 = 0x9114;
pub const GL_SYNC_FLAGS: u32 = 0x9115;
pub const GL_SYNC_FENCE: u32 = 0x9116;
pub const GL_SIGNALED: u32 = 0x9119;
pub const GL_UNSIGNALED: u32 = 0x9118;

// indexed-buffer binding readback pnames (glGetIntegeri_v).
pub const GL_UNIFORM_BUFFER_BINDING: u32 = 0x8A28;
pub const GL_SHADER_STORAGE_BUFFER_BINDING: u32 = 0x90D3;
pub const GL_TRANSFORM_FEEDBACK_BUFFER_BINDING: u32 = 0x8C8F;

/// The texture-unit bank size — the units `glActiveTexture`/`glBindSampler` accept and the value
/// `GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS` advertises (the GLES3 minimum is 32).
pub const MAX_TEXTURE_UNITS: usize = 32;

// glBindBufferBase/Range indexed-target minimum binding caps (ES3.1).
pub const MAX_UNIFORM_BUFFER_BINDINGS: u32 = 24;
pub const MAX_SHADER_STORAGE_BUFFER_BINDINGS: u32 = 8;
pub const MAX_ATOMIC_COUNTER_BUFFER_BINDINGS: u32 = 8;
pub const MAX_TRANSFORM_FEEDBACK_BUFFERS: u32 = 4;

// ES3 sampler-object parameters (glSamplerParameter* / glGetSamplerParameter*).
pub const GL_TEXTURE_WRAP_R: u32 = 0x8072;
pub const GL_TEXTURE_MIN_LOD: u32 = 0x813A;
pub const GL_TEXTURE_MAX_LOD: u32 = 0x813B;
pub const GL_TEXTURE_COMPARE_MODE: u32 = 0x884C;
pub const GL_TEXTURE_COMPARE_FUNC: u32 = 0x884D;
pub const GL_COMPARE_REF_TO_TEXTURE: u32 = 0x884E;

// ES3 array/3D texture targets (glTexStorage3D / glTexImage3D / glTexSubImage3D).
pub const GL_TEXTURE_3D: u32 = 0x806F;
pub const GL_TEXTURE_2D_ARRAY: u32 = 0x8C1A;

// glGetBufferParameteriv pnames.
pub const GL_BUFFER_SIZE: u32 = 0x8764;
pub const GL_BUFFER_USAGE: u32 = 0x8765;

// occlusion / transform-feedback query objects (glBeginQuery / glGetQueryObjectuiv).
pub const GL_ANY_SAMPLES_PASSED: u32 = 0x8C2F;
pub const GL_ANY_SAMPLES_PASSED_CONSERVATIVE: u32 = 0x8D6A;
pub const GL_TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN: u32 = 0x8C88;
pub const GL_CURRENT_QUERY: u32 = 0x8865;
pub const GL_QUERY_RESULT: u32 = 0x8866;
pub const GL_QUERY_RESULT_AVAILABLE: u32 = 0x8867;

// transform-feedback objects (glBindTransformFeedback / glTransformFeedbackVaryings).
pub const GL_TRANSFORM_FEEDBACK: u32 = 0x8E22;
pub const GL_INTERLEAVED_ATTRIBS: u32 = 0x8C8C;
pub const GL_SEPARATE_ATTRIBS: u32 = 0x8C8D;

// separate-shader program pipelines (glProgramParameteri / glUseProgramStages).
pub const GL_PROGRAM_SEPARABLE: u32 = 0x8258;
pub const GL_ACTIVE_PROGRAM: u32 = 0x8259;
pub const GL_VERTEX_SHADER_BIT: u32 = 0x0000_0001;
pub const GL_FRAGMENT_SHADER_BIT: u32 = 0x0000_0002;
pub const GL_COMPUTE_SHADER_BIT: u32 = 0x0000_0020;
pub const GL_ALL_SHADER_BITS: u32 = 0xFFFF_FFFF;

// glGetUniformIndices / glGetActiveUniformsiv per-uniform pnames + the "not an index" sentinel.
pub const GL_INVALID_INDEX: u32 = 0xFFFF_FFFF;
pub const GL_UNIFORM_TYPE: u32 = 0x8A37;
pub const GL_UNIFORM_SIZE: u32 = 0x8A38;
pub const GL_UNIFORM_NAME_LENGTH: u32 = 0x8A39;
pub const GL_UNIFORM_BLOCK_INDEX: u32 = 0x8A3A;
pub const GL_UNIFORM_OFFSET: u32 = 0x8A3B;
pub const GL_UNIFORM_ARRAY_STRIDE: u32 = 0x8A3C;
pub const GL_UNIFORM_MATRIX_STRIDE: u32 = 0x8A3D;
pub const GL_UNIFORM_IS_ROW_MAJOR: u32 = 0x8A3E;

// uniform-block reflection pnames (glGetActiveUniformBlockiv / glUniformBlockBinding).
pub const GL_UNIFORM_BLOCK_BINDING: u32 = 0x8A3F;
pub const GL_UNIFORM_BLOCK_DATA_SIZE: u32 = 0x8A40;
pub const GL_UNIFORM_BLOCK_NAME_LENGTH: u32 = 0x8A41;
pub const GL_UNIFORM_BLOCK_ACTIVE_UNIFORMS: u32 = 0x8A42;
pub const GL_UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES: u32 = 0x8A43;

// program-resource introspection (glGetProgramInterfaceiv / glGetProgramResource*).
pub const GL_UNIFORM: u32 = 0x92E1;
pub const GL_UNIFORM_BLOCK: u32 = 0x92E2;
pub const GL_PROGRAM_INPUT: u32 = 0x92E3;
pub const GL_PROGRAM_OUTPUT: u32 = 0x92E4;
pub const GL_ACTIVE_RESOURCES: u32 = 0x92F5;
pub const GL_MAX_NAME_LENGTH: u32 = 0x92F6;
pub const GL_NAME_LENGTH: u32 = 0x92F9;
pub const GL_TYPE: u32 = 0x92FA;
pub const GL_ARRAY_SIZE: u32 = 0x92FB;
pub const GL_OFFSET: u32 = 0x92FC;
pub const GL_BLOCK_INDEX: u32 = 0x92FD;
pub const GL_LOCATION: u32 = 0x930E;

// glClearBuffer* buffer selectors.
pub const GL_COLOR: u32 = 0x1800;
pub const GL_DEPTH: u32 = 0x1801;
pub const GL_STENCIL: u32 = 0x1802;
pub const GL_DEPTH_STENCIL: u32 = 0x84F9;

// glGetTexLevelParameter* pnames.
pub const GL_TEXTURE_WIDTH: u32 = 0x1000;
pub const GL_TEXTURE_HEIGHT: u32 = 0x1001;
pub const GL_TEXTURE_INTERNAL_FORMAT: u32 = 0x1003;

// glGetRenderbufferParameteriv pnames + the neutral sized internal format.
pub const GL_RENDERBUFFER_WIDTH: u32 = 0x8D42;
pub const GL_RENDERBUFFER_HEIGHT: u32 = 0x8D43;
pub const GL_RENDERBUFFER_INTERNAL_FORMAT: u32 = 0x8D44;
pub const GL_RGBA8: u32 = 0x8058;

// GLES3 sized (and lenient) color/depth internal formats accepted by glTexStorage2D/3D +
// glRenderbufferStorage. glTexStorage* requires a SIZED internal format (the unsized GL_RGB/GL_RGBA are
// accepted leniently for back-compat). Chrome's SharedImage GL-texture backing allocates its compositor
// tiles with GL_RGBA8, so rejecting the sized spellings blanks every tile.
pub const GL_RGB8: u32 = 0x8051;
pub const GL_RGB565: u32 = 0x8D62;
pub const GL_RGBA4: u32 = 0x8056;
pub const GL_RGB5_A1: u32 = 0x8057;
pub const GL_RGB10_A2: u32 = 0x8059;
pub const GL_RGB10_A2UI: u32 = 0x906F;
pub const GL_SRGB8: u32 = 0x8C41;
pub const GL_SRGB8_ALPHA8: u32 = 0x8C43;
pub const GL_BGRA8_EXT: u32 = 0x93A1;
pub const GL_R8: u32 = 0x8229;
pub const GL_RG8: u32 = 0x822B;
pub const GL_R16F: u32 = 0x822D;
pub const GL_RG16F: u32 = 0x822F;
pub const GL_RGB16F: u32 = 0x881B;
pub const GL_RGBA16F: u32 = 0x881A;
pub const GL_R32F: u32 = 0x822E;
pub const GL_RG32F: u32 = 0x8230;
pub const GL_RGBA32F: u32 = 0x8814;
pub const GL_R11F_G11F_B10F: u32 = 0x8C3A;
pub const GL_RGB9_E5: u32 = 0x8C3D;
pub const GL_DEPTH_COMPONENT16: u32 = 0x81A5;
pub const GL_DEPTH_COMPONENT24: u32 = 0x81A6;
pub const GL_DEPTH_COMPONENT32F: u32 = 0x8CAC;
pub const GL_DEPTH32F_STENCIL8: u32 = 0x8CAD;

// glGetFramebufferAttachmentParameteriv pnames + object-type values.
pub const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: u32 = 0x8CD0;
pub const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: u32 = 0x8CD1;
pub const GL_FRAMEBUFFER_DEFAULT: u32 = 0x8218;
pub const GL_TEXTURE: u32 = 0x1702;

// glGetInternalformativ pnames.
pub const GL_NUM_SAMPLE_COUNTS: u32 = 0x9380;

// shader/program compiler + binary capability queries.
pub const GL_SHADER_COMPILER: u32 = 0x8DFA;
pub const GL_NUM_SHADER_BINARY_FORMATS: u32 = 0x8DF9;
pub const GL_NUM_PROGRAM_BINARY_FORMATS: u32 = 0x87FE;
pub const GL_PROGRAM_BINARY_LENGTH: u32 = 0x8741;

// glGetShaderPrecisionFormat precision-type enums (float low/med/high, then int low/med/high).
pub const GL_LOW_FLOAT: u32 = 0x8DF0;
pub const GL_HIGH_FLOAT: u32 = 0x8DF2;
pub const GL_LOW_INT: u32 = 0x8DF3;
pub const GL_HIGH_INT: u32 = 0x8DF5;

// glGetGraphicsResetStatus — no reset.
pub const GL_NO_RESET_NOTIFICATION: u32 = 0x8261;

// framebuffer / renderbuffer objects (offscreen render targets).
pub const GL_FRAMEBUFFER: u32 = 0x8D40;
pub const GL_RENDERBUFFER: u32 = 0x8D41;
pub const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
pub const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
pub const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
pub const GL_DEPTH_ATTACHMENT: u32 = 0x8D00;
pub const GL_STENCIL_ATTACHMENT: u32 = 0x8D20;
pub const GL_DEPTH_STENCIL_ATTACHMENT: u32 = 0x821A;
// framebuffer bindings (GL_DRAW_FRAMEBUFFER_BINDING == GL_FRAMEBUFFER_BINDING, 0x8CA6, above).
pub const GL_READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
pub const GL_RENDERBUFFER_BINDING: u32 = 0x8CA7;
// glCheckFramebufferStatus return values.
pub const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
pub const GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT: u32 = 0x8CD6;
pub const GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT: u32 = 0x8CD7;

/// Byte size of one component of a GL vertex/index element type (`GL_FLOAT` → 4, `GL_UNSIGNED_BYTE` → 1,
/// …). Used to compute the tightly-packed element size when capturing a client-side vertex array (0 stride)
/// and the per-index size of a client-side element array. An unrecognized type defaults to 4 (the safe,
/// widest common case) so a capture never under-reads.
pub struct GlType(pub u32);

impl GlType {
    pub fn component_size(self) -> usize {
        match self.0 {
            GL_BYTE | GL_UNSIGNED_BYTE => 1,
            GL_SHORT | GL_UNSIGNED_SHORT | GL_HALF_FLOAT => 2,
            GL_INT | GL_UNSIGNED_INT | GL_FLOAT => 4,
            _ => 4,
        }
    }
}
