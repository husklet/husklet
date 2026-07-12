//! Pure IR-encoding helpers ported verbatim from `gl_shim.c`: the resource-id maps and the GL→wire
//! enum mappings the swap-time lowering uses. Kept as standalone, unit-tested functions so the future
//! present/draw path (owned elsewhere for now) lowers to *byte-identical* IR as the C shim.
//!
//! These are value maps only — no state, no present coupling — so they are safe to land and test now.

use crate::glconst::*;

// ---- resource id maps (gl_shim.c) --------------------------------------------------------------
// The IR resource-id namespaces the shim assigns per GL object, so the host resource table is stable
// across frames. Ranges must match gl_shim.c exactly.

/// Texture IR id for GL texture `tex` (`tex_ir_id`: 500 + tex).
pub fn tex_ir_id(tex: u32) -> u32 {
    500 + tex
}
/// Sampler IR id for GL texture `tex` (`sampler_ir_id`: 600 + tex).
pub fn sampler_ir_id(tex: u32) -> u32 {
    600 + tex
}
/// Staging-buffer IR id for GL texture `tex` (`stage_ir_id`: 700 + tex).
pub fn stage_ir_id(tex: u32) -> u32 {
    700 + tex
}
/// Per-draw replayed VBO IR id (`replay_vbo_ir_id`: 2000 + draw*MAXATTR + slot).
pub fn replay_vbo_ir_id(draw_index: u32, slot: u32) -> u32 {
    2000 + draw_index * (crate::state::MAXATTR as u32) + slot
}
/// Per-draw replayed index-buffer IR id (`replay_ibo_ir_id`: 10000 + draw).
pub fn replay_ibo_ir_id(draw_index: u32) -> u32 {
    10000 + draw_index
}
/// Color-target texture format selector (`color_target_format`): offscreen GL textures are
/// Rgba8Unorm (1); the IOSurface-backed window is Bgra8Unorm (2).
pub fn color_target_format(target: u32) -> u32 {
    if target != 0 {
        1
    } else {
        2
    }
}

// ---- GL enum -> wire enum (gl_shim.c) ----------------------------------------------------------

/// Blend factor GL enum → wire code (`blend_factor_wire`). Unknown → 1 (One), as in the C shim.
pub fn blend_factor_wire(f: u32) -> u32 {
    match f {
        GL_ZERO => 0,
        GL_ONE => 1,
        GL_SRC_COLOR => 2,
        GL_ONE_MINUS_SRC_COLOR => 3,
        GL_SRC_ALPHA => 4,
        GL_ONE_MINUS_SRC_ALPHA => 5,
        GL_DST_COLOR => 6,
        GL_ONE_MINUS_DST_COLOR => 7,
        GL_DST_ALPHA => 8,
        GL_ONE_MINUS_DST_ALPHA => 9,
        GL_SRC_ALPHA_SATURATE => 10,
        _ => 1,
    }
}

/// Blend equation GL enum → wire code (`blend_op_wire`). Unknown / FUNC_ADD → 0.
pub fn blend_op_wire(e: u32) -> u32 {
    match e {
        GL_FUNC_SUBTRACT => 1,
        GL_FUNC_REVERSE_SUBTRACT => 2,
        GL_MIN => 3,
        GL_MAX => 4,
        _ => 0, // GL_FUNC_ADD and unknown
    }
}

/// Vertex-attribute format packing (`vertex_format_wire`):
/// `comps | (kind<<8) | (normalized<<16) | (integer<<17)`, comps clamped to [1,4].
pub fn vertex_format_wire(kind_enum: u32, comps: i32, normalized: bool, integer: bool) -> u32 {
    let comps = comps.clamp(1, 4) as u32;
    let kind = match kind_enum {
        GL_UNSIGNED_BYTE => 1,
        GL_BYTE => 2,
        GL_UNSIGNED_SHORT => 3,
        GL_SHORT => 4,
        GL_UNSIGNED_INT => 5,
        GL_INT => 6,
        _ => 0, // GL_FLOAT and unknown
    };
    comps | (kind << 8) | ((normalized as u32) << 16) | ((integer as u32) << 17)
}

/// Vertex-attribute format from an MSL/GLSL declaration type string (`decl_format_wire`).
pub fn decl_format_wire(t: &str) -> u32 {
    let comps: u32 = if t.contains("vec2") {
        2
    } else if t.contains("vec3") {
        3
    } else if t.starts_with("float") {
        1
    } else {
        4
    };
    let integer = t.starts_with("ivec") || t.starts_with("uvec");
    let kind: u32 = if t.starts_with("ivec") {
        6
    } else if t.starts_with("uvec") {
        5
    } else {
        0
    };
    comps | (kind << 8) | ((integer as u32) << 17)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Parity with gl_shim.c's exact id ranges and wire codes. These constants are the wire ABI; if a
    // value here changes, the host decoder + the C shim disagree.
    #[test]
    fn id_maps_match_c_shim() {
        assert_eq!(tex_ir_id(3), 503);
        assert_eq!(sampler_ir_id(3), 603);
        assert_eq!(stage_ir_id(3), 703);
        assert_eq!(replay_vbo_ir_id(2, 5), 2000 + 2 * 16 + 5);
        assert_eq!(replay_ibo_ir_id(7), 10007);
        assert_eq!(color_target_format(0), 2);
        assert_eq!(color_target_format(9), 1);
    }

    #[test]
    fn blend_factor_wire_matches_c_shim() {
        let cases = [
            (GL_ZERO, 0),
            (GL_ONE, 1),
            (GL_SRC_COLOR, 2),
            (GL_ONE_MINUS_SRC_COLOR, 3),
            (GL_SRC_ALPHA, 4),
            (GL_ONE_MINUS_SRC_ALPHA, 5),
            (GL_DST_COLOR, 6),
            (GL_ONE_MINUS_DST_COLOR, 7),
            (GL_DST_ALPHA, 8),
            (GL_ONE_MINUS_DST_ALPHA, 9),
            (GL_SRC_ALPHA_SATURATE, 10),
            (0xDEAD, 1), // unknown → One
        ];
        for (gl, wire) in cases {
            assert_eq!(blend_factor_wire(gl), wire, "blend factor 0x{gl:x}");
        }
    }

    #[test]
    fn blend_op_wire_matches_c_shim() {
        assert_eq!(blend_op_wire(GL_FUNC_ADD), 0);
        assert_eq!(blend_op_wire(GL_FUNC_SUBTRACT), 1);
        assert_eq!(blend_op_wire(GL_FUNC_REVERSE_SUBTRACT), 2);
        assert_eq!(blend_op_wire(GL_MIN), 3);
        assert_eq!(blend_op_wire(GL_MAX), 4);
        assert_eq!(blend_op_wire(0xDEAD), 0);
    }

    #[test]
    fn vertex_format_wire_matches_c_shim() {
        // float3, not normalized, not integer.
        assert_eq!(vertex_format_wire(GL_FLOAT, 3, false, false), 3 | (0 << 8));
        // ubyte4 normalized (glmark2 color attribs).
        assert_eq!(vertex_format_wire(GL_UNSIGNED_BYTE, 4, true, false), 4 | (1 << 8) | (1 << 16));
        // int2 integer.
        assert_eq!(vertex_format_wire(GL_INT, 2, false, true), 2 | (6 << 8) | (1 << 17));
        // comps clamp.
        assert_eq!(vertex_format_wire(GL_FLOAT, 9, false, false), 4);
        assert_eq!(vertex_format_wire(GL_FLOAT, 0, false, false), 1);
    }

    #[test]
    fn decl_format_wire_matches_c_shim() {
        assert_eq!(decl_format_wire("vec2"), 2);
        assert_eq!(decl_format_wire("vec3"), 3);
        assert_eq!(decl_format_wire("float"), 1);
        assert_eq!(decl_format_wire("vec4"), 4);
        assert_eq!(decl_format_wire("ivec3"), 3 | (6 << 8) | (1 << 17));
        assert_eq!(decl_format_wire("uvec4"), 4 | (5 << 8) | (1 << 17));
    }
}
