//! The EGL config table — the single, truthful `EGLConfig` this driver advertises and its attributes.
//!
//! `eglGetConfigs` / `eglChooseConfig` / `eglGetConfigAttrib` are pure marshalling in the guest cdylib
//! (`shim/egl`): the honest values live HERE so they are unit-testable without the shim (the same split
//! [`crate::service::query`] uses for the `glGetString` identity). This driver backs exactly ONE config —
//! an 8/8/8/8 RGBA color buffer with a 24-bit depth + 8-bit stencil buffer, renderable by OpenGL ES 2 and
//! ES 3, usable for both window and pbuffer surfaces — so the enumeration is a one-element table.
//!
//! The attribute values are the driver's REAL capabilities (see [`crate::service::query`] for the
//! matching `glGetIntegerv` limits), not padding: the color/depth/stencil sizes match what
//! `eglGetConfigAttrib(EGL_*_SIZE)` and the GL `GL_*_BITS` queries report, and the surface / renderable
//! bits match the surface-creation + context entry points the shim actually implements.

// ---- EGL config attribute enums (EGL 1.5 core; values from `<EGL/egl.h>`) -------------------------

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
pub const EGL_TRANSPARENT_BLUE_VALUE: i32 = 0x3035;
pub const EGL_TRANSPARENT_GREEN_VALUE: i32 = 0x3036;
pub const EGL_TRANSPARENT_RED_VALUE: i32 = 0x3037;
pub const EGL_NONE: i32 = 0x3038;
pub const EGL_BIND_TO_TEXTURE_RGB: i32 = 0x3039;
pub const EGL_BIND_TO_TEXTURE_RGBA: i32 = 0x303A;
pub const EGL_MIN_SWAP_INTERVAL: i32 = 0x303B;
pub const EGL_MAX_SWAP_INTERVAL: i32 = 0x303C;
pub const EGL_LUMINANCE_SIZE: i32 = 0x303D;
pub const EGL_ALPHA_MASK_SIZE: i32 = 0x303E;
pub const EGL_COLOR_BUFFER_TYPE: i32 = 0x303F;
pub const EGL_RENDERABLE_TYPE: i32 = 0x3040;
pub const EGL_CONFORMANT: i32 = 0x3042;

// Attribute VALUE enums.
pub const EGL_RGB_BUFFER: i32 = 0x308E; // EGL_COLOR_BUFFER_TYPE value
pub const EGL_WINDOW_BIT: i32 = 0x0004;
pub const EGL_PBUFFER_BIT: i32 = 0x0001;
pub const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
pub const EGL_OPENGL_ES3_BIT: i32 = 0x0040;

// ---- the advertised config table -----------------------------------------------------------------

/// The number of `EGLConfig`s this driver advertises (`eglGetConfigs` total; the length of the table
/// `eglChooseConfig` selects from). Exactly one.
pub const NUM_CONFIGS: i32 = 1;
/// The opaque, non-zero id of the single config (`eglGetConfigAttrib(EGL_CONFIG_ID)`). EGL config ids are
/// 1-based, so `0` is never a valid id.
pub const CONFIG_ID: i32 = 1;

// The color / depth / stencil sizes — the driver's REAL surface format (matches `GL_*_BITS`).
const RED: i32 = 8;
const GREEN: i32 = 8;
const BLUE: i32 = 8;
const ALPHA: i32 = 8;
const DEPTH: i32 = 24;
const STENCIL: i32 = 8;
const BUFFER: i32 = RED + GREEN + BLUE + ALPHA; // 32-bit color buffer
const MAX_PBUFFER: i32 = 16384; // matches query::MAX_TEXTURE_SIZE / VIEWPORT_DIM

/// Why `config_attrib` rejected a query — mapped by the shim to the EGL error it raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// The queried `EGLConfig` handle is not one this driver handed out (`EGL_BAD_CONFIG`).
    BadConfig,
    /// The attribute enum is not a valid `EGLConfig` attribute (`EGL_BAD_ATTRIBUTE`).
    BadAttribute,
}

/// `eglGetConfigAttrib(config, attribute)` — the truthful value of `attribute` for our single config.
///
/// * `is_our_config` — whether the queried handle is the one this driver advertises. A foreign handle is
///   [`ConfigError::BadConfig`] (the shim must NOT dereference an unknown config — it has no backing).
/// * a recognized `EGLConfig` attribute returns its real value; an unrecognized enum is
///   [`ConfigError::BadAttribute`] (never a silent `0`, so a caller probing an attribute learns the truth
///   instead of reading a fabricated value).
pub struct Config;

impl Config {
    pub fn attrib(is_our_config: bool, attribute: i32) -> Result<i32, ConfigError> {
        if !is_our_config {
            return Err(ConfigError::BadConfig);
        }
        let v = match attribute {
            EGL_CONFIG_ID => CONFIG_ID,
            EGL_BUFFER_SIZE => BUFFER,
            EGL_RED_SIZE => RED,
            EGL_GREEN_SIZE => GREEN,
            EGL_BLUE_SIZE => BLUE,
            EGL_ALPHA_SIZE => ALPHA,
            EGL_LUMINANCE_SIZE => 0,
            EGL_ALPHA_MASK_SIZE => 0,
            EGL_DEPTH_SIZE => DEPTH,
            EGL_STENCIL_SIZE => STENCIL,
            EGL_SAMPLES => 0,
            EGL_SAMPLE_BUFFERS => 0,
            EGL_COLOR_BUFFER_TYPE => EGL_RGB_BUFFER,
            EGL_CONFIG_CAVEAT => EGL_NONE,
            EGL_CONFORMANT | EGL_RENDERABLE_TYPE => EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT,
            EGL_SURFACE_TYPE => EGL_WINDOW_BIT | EGL_PBUFFER_BIT,
            EGL_LEVEL => 0,
            EGL_MAX_PBUFFER_WIDTH | EGL_MAX_PBUFFER_HEIGHT => MAX_PBUFFER,
            EGL_MAX_PBUFFER_PIXELS => MAX_PBUFFER * MAX_PBUFFER,
            EGL_NATIVE_RENDERABLE => 0, // EGL_FALSE
            EGL_NATIVE_VISUAL_ID => 0,
            EGL_NATIVE_VISUAL_TYPE => EGL_NONE,
            EGL_MIN_SWAP_INTERVAL => 0,
            EGL_MAX_SWAP_INTERVAL => 1,
            EGL_TRANSPARENT_TYPE => EGL_NONE,
            EGL_TRANSPARENT_RED_VALUE
            | EGL_TRANSPARENT_GREEN_VALUE
            | EGL_TRANSPARENT_BLUE_VALUE => 0,
            EGL_BIND_TO_TEXTURE_RGB | EGL_BIND_TO_TEXTURE_RGBA => 0, // EGL_FALSE
            _ => return Err(ConfigError::BadAttribute),
        };
        Ok(v)
    }
}

/// `eglGetConfigs` / `eglChooseConfig` enumeration contract, resolved without touching raw pointers.
///
/// Given whether the caller passed a real `configs` output array (`buffer_present`) and its capacity
/// (`config_size`), returns `(n_to_write, num_config)`:
///   * a NULL array is the "just tell me how many" query — write nothing, report the TOTAL config count;
///   * a real array is filled with `min(config_size, total)` config handles, and `num_config` reports
///     exactly how many were written (a `config_size` of `0`, or negative, writes none).
impl Config {
    pub fn enumerate(buffer_present: bool, config_size: i32) -> (usize, i32) {
        if !buffer_present {
            (0, NUM_CONFIGS)
        } else {
            let n = NUM_CONFIGS.min(config_size.max(0));
            (n as usize, n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_config_reports_truthful_color_depth_stencil() {
        assert_eq!(Config::attrib(true, EGL_RED_SIZE), Ok(8));
        assert_eq!(Config::attrib(true, EGL_GREEN_SIZE), Ok(8));
        assert_eq!(Config::attrib(true, EGL_BLUE_SIZE), Ok(8));
        assert_eq!(Config::attrib(true, EGL_ALPHA_SIZE), Ok(8));
        assert_eq!(Config::attrib(true, EGL_BUFFER_SIZE), Ok(32));
        assert_eq!(Config::attrib(true, EGL_DEPTH_SIZE), Ok(24));
        assert_eq!(Config::attrib(true, EGL_STENCIL_SIZE), Ok(8));
        assert_eq!(Config::attrib(true, EGL_CONFIG_ID), Ok(CONFIG_ID));
        assert_eq!(
            Config::attrib(true, EGL_COLOR_BUFFER_TYPE),
            Ok(EGL_RGB_BUFFER)
        );
        assert_eq!(Config::attrib(true, EGL_CONFIG_CAVEAT), Ok(EGL_NONE));
        assert_eq!(
            Config::attrib(true, EGL_RENDERABLE_TYPE),
            Ok(EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT)
        );
        assert_eq!(
            Config::attrib(true, EGL_SURFACE_TYPE),
            Ok(EGL_WINDOW_BIT | EGL_PBUFFER_BIT)
        );
    }

    #[test]
    fn unknown_attribute_is_bad_attribute_not_a_fake_zero() {
        // A garbage enum must be reported, never silently answered with 0.
        assert_eq!(
            Config::attrib(true, 0x1234_5678),
            Err(ConfigError::BadAttribute)
        );
        assert_eq!(Config::attrib(true, 0), Err(ConfigError::BadAttribute));
    }

    #[test]
    fn foreign_config_handle_is_bad_config() {
        assert_eq!(
            Config::attrib(false, EGL_RED_SIZE),
            Err(ConfigError::BadConfig)
        );
    }

    #[test]
    fn enumerate_null_buffer_reports_count_only() {
        // configs == NULL: write nothing, report the total.
        assert_eq!(Config::enumerate(false, 0), (0, NUM_CONFIGS));
        assert_eq!(Config::enumerate(false, 99), (0, NUM_CONFIGS));
    }

    #[test]
    fn enumerate_bounded_copy() {
        // Real array with room: write + report all configs.
        assert_eq!(Config::enumerate(true, 16), (1, 1));
        assert_eq!(Config::enumerate(true, 1), (1, 1));
        // Zero / negative capacity writes none (no OOB store into a zero-length array).
        assert_eq!(Config::enumerate(true, 0), (0, 0));
        assert_eq!(Config::enumerate(true, -1), (0, 0));
    }
}
