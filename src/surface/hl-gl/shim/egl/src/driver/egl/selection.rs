//! EGL config selection: `eglChooseConfig` / `eglGetConfigs` / `eglGetConfigAttrib` and the opaque
//! `EGLConfig` handle the guest holds.

use super::*;

// ==================================================================================================
// EGL: config selection
// ==================================================================================================

/// Write the given config handles into the caller's `configs` array and report `num_config`. Shared by
/// `eglChooseConfig` / `eglGetConfigs`: a null array is the count-only query, a real array takes a bounded
/// copy and `num_config` reports how many handles were written.
unsafe fn write_configs(
    ids: &[i32],
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) {
    let written = if configs.is_null() {
        0
    } else {
        ids.len().min(config_size.max(0) as usize)
    };
    for (index, id) in ids.iter().take(written).enumerate() {
        *configs.add(index) = ConfigHandle::of(*id);
    }
    if !num_config.is_null() {
        // A null array reports how many configs are available; a real array reports how many were written.
        *num_config = if configs.is_null() {
            ids.len() as i32
        } else {
            written as i32
        };
    }
}

/// The opaque `EGLConfig` a guest holds: the config's own 1-based id (`0` is never a valid id, so the
/// handle is never null). Carrying the id — rather than one fixed token — is what lets the driver expose
/// every entry of [`config::CONFIGS`].
pub(in crate::driver) struct ConfigHandle;

impl ConfigHandle {
    fn of(id: i32) -> *mut c_void {
        id as usize as *mut c_void
    }

    /// The ids the guest is allowed to see: the first [`config::NUM_CONFIGS`] entries of the honest table.
    /// The service owns how many of its configs are exposed, so widening the set is a one-constant change
    /// there and needs nothing here.
    fn exposed() -> Vec<i32> {
        config::Config::ids()
            .take(config::NUM_CONFIGS.max(0) as usize)
            .collect()
    }

    /// The config id a caller-supplied handle denotes, or `None` when this driver never handed it out.
    pub(in crate::driver) fn id(config: *mut c_void) -> Option<i32> {
        let id = config as usize as i32;
        Self::exposed().into_iter().find(|advertised| *advertised == id)
    }
}

/// An `eglChooseConfig` attribute list decoded into `(attribute, value)` pairs.
struct ConfigAttributeList;

impl ConfigAttributeList {
    /// Decode the caller's `EGL_NONE`-terminated list.
    ///
    /// DELIBERATE DIVERGENCE, do not "fix" silently: EGL 1.5 §3.4.1 defaults `EGL_RENDERABLE_TYPE` to
    /// `EGL_OPENGL_ES_BIT` (ES 1.x), which no config here claims because this driver implements no ES 1
    /// pipeline — so a strictly-read match-all list would match NOTHING and break every caller that passes
    /// NULL (toolkit probes, `eglinfo`, this product's own fixtures). When the caller does not name
    /// `EGL_RENDERABLE_TYPE`, this driver reads that as "any API this driver serves" and substitutes
    /// `ES2|ES3`. Every other default stays exactly as specified.
    fn parse(attrib_list: *const i32) -> Vec<(i32, i32)> {
        let mut pairs = Vec::new();
        if !attrib_list.is_null() {
            for index in 0..64 {
                // SAFETY: an EGL attribute list is a caller-supplied `EGL_NONE`-terminated pair sequence;
                // the value is read only after a non-terminator key is seen.
                let attribute = unsafe { *attrib_list.add(index * 2) };
                if attribute == EGL_NONE {
                    break;
                }
                let value = unsafe { *attrib_list.add(index * 2 + 1) };
                pairs.push((attribute, value));
            }
        }
        if !pairs
            .iter()
            .any(|(attribute, _)| *attribute == config::EGL_RENDERABLE_TYPE)
        {
            pairs.push((
                config::EGL_RENDERABLE_TYPE,
                config::EGL_OPENGL_ES2_BIT | config::EGL_OPENGL_ES3_BIT,
            ));
        }
        pairs
    }
}

/// `eglChooseConfig(dpy, attrib_list, configs, config_size, num_config)` — select configs matching the
/// requested attributes. This driver advertises one config that satisfies the common GLES2/ES3 window +
/// pbuffer requests, so a null / empty `attrib_list` (match-all) and a populated list both return it. A
/// null `configs` array is the count-only query (`num_config` = number available). `num_config` is
/// required by the spec; a null one is `EGL_BAD_PARAMETER`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglChooseConfig(
    _dpy: *mut c_void,
    attrib_list: *const i32,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    crate::stub::trace("eglChooseConfig", "selecting hl configuration");
    if num_config.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let exposed = ConfigHandle::exposed();
    let matched: Vec<i32> = config::Config::choose(&ConfigAttributeList::parse(attrib_list))
        .into_iter()
        .filter(|id| exposed.contains(id))
        .collect();
    unsafe { write_configs(&matched, configs, config_size, num_config) };
    EGL_TRUE
}

/// `eglGetConfigs(dpy, configs, config_size, num_config)` — enumerate all configs. A null `configs` array
/// returns the total count in `num_config`; a real array is filled with up to `config_size` handles and
/// `num_config` reports how many were written. `num_config` is required; a null one is
/// `EGL_BAD_PARAMETER`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetConfigs(
    _dpy: *mut c_void,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    if num_config.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let ids = ConfigHandle::exposed();
    unsafe { write_configs(&ids, configs, config_size, num_config) };
    EGL_TRUE
}

/// `eglGetConfigAttrib(dpy, config, attribute, value)` — the value of one attribute of an `EGLConfig`.
/// Delegates the truthful value to [`config::config_attrib`]: a foreign config handle raises
/// `EGL_BAD_CONFIG` and an unrecognized attribute raises `EGL_BAD_ATTRIBUTE` — both return `EGL_FALSE`
/// WITHOUT writing `value` or dereferencing the unknown config, instead of the old silent `0`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetConfigAttrib(
    _dpy: *mut c_void,
    config: *mut c_void,
    attribute: i32,
    value: *mut i32,
) -> u32 {
    if value.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let attrib = match ConfigHandle::id(config) {
        Some(id) => config::Config::attrib_of(id, attribute),
        None => Err(config::ConfigError::BadConfig),
    };
    match attrib {
        Ok(v) => {
            unsafe { *value = v };
            EGL_TRUE
        }
        Err(config::ConfigError::BadConfig) => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
            EGL_FALSE
        }
        Err(config::ConfigError::BadAttribute) => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            EGL_FALSE
        }
    }
}
