//! GL/EGL enum constants used by the recording ops and the frame builder. Values are the canonical
//! Khronos numbers (identical to `hl-shim-gl/src/glconst.rs` and `gl_shim.c`'s `#define`s); kept in one
//! place so the state machine and services reference names, not magic hex.
#![allow(dead_code)]

// data types (glVertexAttribPointer / glDrawElements index type)
pub const GL_BYTE: u32 = 0x1400;
pub const GL_UNSIGNED_BYTE: u32 = 0x1401;
pub const GL_SHORT: u32 = 0x1402;
pub const GL_UNSIGNED_SHORT: u32 = 0x1403;
pub const GL_INT: u32 = 0x1404;
pub const GL_UNSIGNED_INT: u32 = 0x1405;
pub const GL_FLOAT: u32 = 0x1406;
pub const GL_HALF_FLOAT: u32 = 0x140B;

// buffer targets
pub const GL_ARRAY_BUFFER: u32 = 0x8892;
pub const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;

// primitive topology + clear mask
pub const GL_POINTS: u32 = 0x0000;
pub const GL_LINES: u32 = 0x0001;
pub const GL_LINE_STRIP: u32 = 0x0003;
pub const GL_TRIANGLES: u32 = 0x0004;
pub const GL_TRIANGLE_STRIP: u32 = 0x0005;
pub const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
pub const GL_DEPTH_BUFFER_BIT: u32 = 0x0100;

// caps toggled by glEnable/glDisable
pub const GL_DEPTH_TEST: u32 = 0x0B71;
pub const GL_BLEND: u32 = 0x0BE2;
pub const GL_CULL_FACE: u32 = 0x0B44;
pub const GL_SCISSOR_TEST: u32 = 0x0C11;

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
pub const GL_NEAREST: u32 = 0x2600;
pub const GL_LINEAR: u32 = 0x2601;
pub const GL_NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
pub const GL_LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
pub const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
pub const GL_LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
pub const GL_CLAMP_TO_EDGE: u32 = 0x812F;
pub const GL_REPEAT: u32 = 0x2901;
pub const GL_MIRRORED_REPEAT: u32 = 0x8370;
