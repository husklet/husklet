//! Behavioural tests for the advertised config table, its attributes, and `eglChooseConfig` selection.

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

/// glmark2 at DEFAULT settings: it asks for 8-bit color, a depth buffer and explicitly NO stencil.
/// The old single stencil-8 table gave it nothing acceptable; the honest set must offer a stencil-0
/// config AND rank it first.
#[test]
fn a_default_request_for_no_stencil_selects_a_stencil_free_config() {
    let chosen = Config::choose(&[
        (EGL_RED_SIZE, 8),
        (EGL_GREEN_SIZE, 8),
        (EGL_BLUE_SIZE, 8),
        (EGL_ALPHA_SIZE, 1),
        (EGL_DEPTH_SIZE, 1),
        (EGL_STENCIL_SIZE, 0),
        (EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT),
    ]);
    assert!(!chosen.is_empty(), "a stencil-less request must match");
    let first = chosen[0];
    assert_eq!(
        Config::attrib_of(first, EGL_STENCIL_SIZE),
        Ok(0),
        "the specification sorts the smaller stencil first"
    );
    assert!(
        Config::attrib_of(first, EGL_DEPTH_SIZE).unwrap() >= 1,
        "a depth buffer was requested"
    );
}

/// A request that needs stencil still gets the stencil-8 config, and only that one.
#[test]
fn a_stencil_request_selects_only_configs_that_have_stencil() {
    let chosen = Config::choose(&[
        (EGL_STENCIL_SIZE, 8),
        (EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT),
    ]);
    assert!(!chosen.is_empty());
    for id in chosen {
        assert!(Config::attrib_of(id, EGL_STENCIL_SIZE).unwrap() >= 8);
    }
}

/// The size attributes are "at least" matches: a 5/6/5 request is satisfied by the 8/8/8 buffer, and a
/// depth-16 request by the 24-bit one.
#[test]
fn size_attributes_match_at_least_not_exactly() {
    let chosen = Config::choose(&[
        (EGL_RED_SIZE, 5),
        (EGL_GREEN_SIZE, 6),
        (EGL_BLUE_SIZE, 5),
        (EGL_DEPTH_SIZE, 16),
        (EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT),
    ]);
    assert!(!chosen.is_empty());
    for id in &chosen {
        assert_eq!(Config::attrib_of(*id, EGL_RED_SIZE), Ok(8));
        assert!(Config::attrib_of(*id, EGL_DEPTH_SIZE).unwrap() >= 16);
    }
}

/// Sorting: for a request that constrains nothing, the ordering is smaller depth then smaller stencil,
/// then config id — and every advertised config is returned.
#[test]
fn unconstrained_selection_returns_every_config_in_spec_sort_order() {
    let chosen = Config::choose(&[(EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT)]);
    assert_eq!(chosen.len(), CONFIGS.len(), "nothing was filtered out");
    let keys: Vec<(i32, i32)> = chosen
        .iter()
        .map(|id| {
            (
                Config::attrib_of(*id, EGL_DEPTH_SIZE).unwrap(),
                Config::attrib_of(*id, EGL_STENCIL_SIZE).unwrap(),
            )
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "ascending depth then stencil");
}

/// Bitmask attributes must be SUBSET matches, and the specification's default
/// `EGL_RENDERABLE_TYPE` is ES1 — which this driver does not implement, so an empty attribute list
/// honestly matches nothing rather than handing back an ES2-only config.
#[test]
fn renderable_and_surface_bits_are_subset_matches() {
    assert!(
        Config::choose(&[]).is_empty(),
        "the ES1 default matches nothing"
    );
    assert!(!Config::choose(&[(EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT)]).is_empty());
    assert!(
        Config::choose(&[(EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES_BIT)]).is_empty(),
        "an ES1 bit in the request cannot be satisfied"
    );
    assert!(
        Config::choose(&[
            (EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT),
            (EGL_SURFACE_TYPE, EGL_WINDOW_BIT | EGL_PBUFFER_BIT),
        ])
        .len()
            == CONFIGS.len()
    );
}

/// An unsatisfiable request yields NO config instead of a config that would fail later: this driver
/// has no multisampled default framebuffer and no native pixmaps.
#[test]
fn unsatisfiable_requests_match_nothing() {
    assert!(
        Config::choose(&[(EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT), (EGL_SAMPLES, 4),]).is_empty()
    );
    assert!(Config::choose(&[
        (EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT),
        (EGL_MATCH_NATIVE_PIXMAP, 7),
    ])
    .is_empty());
}

/// Every advertised id answers every attribute; a foreign id is `EGL_BAD_CONFIG`.
#[test]
fn every_advertised_id_answers_attributes_and_a_foreign_id_does_not() {
    for id in Config::ids() {
        assert_eq!(Config::attrib_of(id, EGL_CONFIG_ID), Ok(id));
        assert_eq!(Config::attrib_of(id, EGL_BUFFER_SIZE), Ok(32));
    }
    assert_eq!(
        Config::attrib_of(9999, EGL_RED_SIZE),
        Err(ConfigError::BadConfig)
    );
}
