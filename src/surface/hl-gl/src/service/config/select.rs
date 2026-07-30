//! `eglChooseConfig` attribute matching + the specification's config sort order.

use super::*;

/// `eglChooseConfig` attribute matching + sort order (EGL 1.5 §3.4.1), resolved on the honest table.
///
/// `attributes` is the caller's `attrib_list` already decoded into `(attribute, value)` pairs (the shim
/// stops at `EGL_NONE`). Returns the matching config ids in the specification's sort order, so the caller
/// simply writes the prefix that fits its array.
impl Config {
    /// Every advertised config id, in table order (`eglGetConfigs`).
    pub fn ids() -> impl Iterator<Item = i32> {
        CONFIGS.iter().map(|spec| spec.id)
    }

    pub fn choose(attributes: &[(i32, i32)]) -> Vec<i32> {
        let mut matched: Vec<&ConfigSpec> = CONFIGS
            .iter()
            .filter(|spec| Self::matches(spec, attributes))
            .collect();
        matched.sort_by_key(|spec| Self::sort_key(spec, attributes));
        matched.iter().map(|spec| spec.id).collect()
    }

    /// Whether `spec` satisfies every requested attribute. `EGL_DONT_CARE` and an attribute the caller
    /// omitted impose nothing beyond the specification's defaults.
    fn matches(spec: &ConfigSpec, attributes: &[(i32, i32)]) -> bool {
        let requested = |attribute: i32| {
            attributes
                .iter()
                .rev()
                .find(|(name, _)| *name == attribute)
                .map(|(_, value)| *value)
        };
        // Bitmask attributes: every requested bit must be present in the config's mask.
        for (attribute, default) in [
            (EGL_SURFACE_TYPE, EGL_WINDOW_BIT),
            (EGL_RENDERABLE_TYPE, EGL_OPENGL_ES_BIT),
            (EGL_CONFORMANT, 0),
        ] {
            let wanted = requested(attribute).unwrap_or(default);
            if wanted == EGL_DONT_CARE {
                continue;
            }
            let Ok(advertised) = Self::attrib_of(spec.id, attribute) else {
                return false;
            };
            if wanted & !advertised != 0 {
                return false;
            }
        }
        // "At least" attributes: the config must offer no fewer bits/samples than requested.
        for attribute in [
            EGL_BUFFER_SIZE,
            EGL_RED_SIZE,
            EGL_GREEN_SIZE,
            EGL_BLUE_SIZE,
            EGL_ALPHA_SIZE,
            EGL_LUMINANCE_SIZE,
            EGL_ALPHA_MASK_SIZE,
            EGL_DEPTH_SIZE,
            EGL_STENCIL_SIZE,
            EGL_SAMPLES,
            EGL_SAMPLE_BUFFERS,
        ] {
            let wanted = requested(attribute).unwrap_or(0);
            if wanted == EGL_DONT_CARE {
                continue;
            }
            let Ok(advertised) = Self::attrib_of(spec.id, attribute) else {
                return false;
            };
            if advertised < wanted {
                return false;
            }
        }
        // Exact-match attributes, with the specification's defaults for the ones a caller omits.
        for (attribute, default) in [
            (EGL_CONFIG_ID, EGL_DONT_CARE),
            (EGL_CONFIG_CAVEAT, EGL_DONT_CARE),
            (EGL_COLOR_BUFFER_TYPE, EGL_RGB_BUFFER),
            (EGL_LEVEL, 0),
            (EGL_NATIVE_RENDERABLE, EGL_DONT_CARE),
            (EGL_NATIVE_VISUAL_TYPE, EGL_DONT_CARE),
            (EGL_TRANSPARENT_TYPE, EGL_NONE),
            (EGL_TRANSPARENT_RED_VALUE, EGL_DONT_CARE),
            (EGL_TRANSPARENT_GREEN_VALUE, EGL_DONT_CARE),
            (EGL_TRANSPARENT_BLUE_VALUE, EGL_DONT_CARE),
            (EGL_BIND_TO_TEXTURE_RGB, EGL_DONT_CARE),
            (EGL_BIND_TO_TEXTURE_RGBA, EGL_DONT_CARE),
            (EGL_MIN_SWAP_INTERVAL, EGL_DONT_CARE),
            (EGL_MAX_SWAP_INTERVAL, EGL_DONT_CARE),
        ] {
            let wanted = requested(attribute).unwrap_or(default);
            if wanted == EGL_DONT_CARE {
                continue;
            }
            let Ok(advertised) = Self::attrib_of(spec.id, attribute) else {
                return false;
            };
            if advertised != wanted {
                return false;
            }
        }
        // A native-pixmap match is impossible here: this driver has no native pixmaps.
        !attributes
            .iter()
            .any(|(name, value)| *name == EGL_MATCH_NATIVE_PIXMAP && *value != 0)
    }

    /// The specification's sort key: caveat, then color-buffer type, then the LARGER requested-component
    /// color total (negated so it sorts first), then smaller buffer size, sample buffers, samples, depth,
    /// stencil, alpha-mask, and finally the config id.
    fn sort_key(spec: &ConfigSpec, attributes: &[(i32, i32)]) -> [i32; 9] {
        let attrib = |attribute: i32| Self::attrib_of(spec.id, attribute).unwrap_or(0);
        let mut color = 0;
        for attribute in [EGL_RED_SIZE, EGL_GREEN_SIZE, EGL_BLUE_SIZE, EGL_ALPHA_SIZE] {
            // A component the caller did not ask for (or asked for as 0 / EGL_DONT_CARE) is not counted.
            let wanted = attributes
                .iter()
                .rev()
                .find(|(name, _)| *name == attribute)
                .map(|(_, value)| *value)
                .unwrap_or(0);
            if wanted > 0 {
                color += attrib(attribute);
            }
        }
        [
            attrib(EGL_CONFIG_CAVEAT),
            attrib(EGL_COLOR_BUFFER_TYPE),
            -color,
            attrib(EGL_BUFFER_SIZE),
            attrib(EGL_SAMPLE_BUFFERS),
            attrib(EGL_SAMPLES),
            spec.depth,
            spec.stencil,
            spec.id,
        ]
    }
}
