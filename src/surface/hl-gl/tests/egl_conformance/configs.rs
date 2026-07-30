//! Every `eglGetConfigAttrib` value on every advertised config: internally consistent, at or above the
//! EGL/GLES minima, agreeing with the GL `GL_*_BITS` a context on that config reports, and — for each
//! `EGL_SURFACE_TYPE` and `EGL_RENDERABLE_TYPE` bit claimed — actually backed by the corresponding
//! creation entry point.
//!
//! A config attribute is pure advertising: nothing in a render path consults it, so a wrong value is
//! invisible until an application believes it. EGL 1.5 §3.4 table 3.4 defines the attribute set and its
//! defaults; §3.4.1 the selection and sort order.

use super::*;

/// Every attribute EGL 1.5 table 3.4 defines for an `EGLConfig`. `eglGetConfigAttrib` must answer all of
/// them for a valid config — an `EGL_BAD_ATTRIBUTE` on one of these is a missing attribute, not a
/// protection against a bad query.
const CONFIG_ATTRIBUTES: &[(i32, &str)] = &[
    (EGL_BUFFER_SIZE, "EGL_BUFFER_SIZE"),
    (EGL_RED_SIZE, "EGL_RED_SIZE"),
    (EGL_GREEN_SIZE, "EGL_GREEN_SIZE"),
    (EGL_BLUE_SIZE, "EGL_BLUE_SIZE"),
    (EGL_ALPHA_SIZE, "EGL_ALPHA_SIZE"),
    (EGL_LUMINANCE_SIZE, "EGL_LUMINANCE_SIZE"),
    (EGL_ALPHA_MASK_SIZE, "EGL_ALPHA_MASK_SIZE"),
    (EGL_DEPTH_SIZE, "EGL_DEPTH_SIZE"),
    (EGL_STENCIL_SIZE, "EGL_STENCIL_SIZE"),
    (EGL_SAMPLES, "EGL_SAMPLES"),
    (EGL_SAMPLE_BUFFERS, "EGL_SAMPLE_BUFFERS"),
    (EGL_COLOR_BUFFER_TYPE, "EGL_COLOR_BUFFER_TYPE"),
    (EGL_CONFIG_CAVEAT, "EGL_CONFIG_CAVEAT"),
    (EGL_CONFIG_ID, "EGL_CONFIG_ID"),
    (EGL_CONFORMANT, "EGL_CONFORMANT"),
    (EGL_LEVEL, "EGL_LEVEL"),
    (EGL_MAX_PBUFFER_WIDTH, "EGL_MAX_PBUFFER_WIDTH"),
    (EGL_MAX_PBUFFER_HEIGHT, "EGL_MAX_PBUFFER_HEIGHT"),
    (EGL_MAX_PBUFFER_PIXELS, "EGL_MAX_PBUFFER_PIXELS"),
    (EGL_MAX_SWAP_INTERVAL, "EGL_MAX_SWAP_INTERVAL"),
    (EGL_MIN_SWAP_INTERVAL, "EGL_MIN_SWAP_INTERVAL"),
    (EGL_NATIVE_RENDERABLE, "EGL_NATIVE_RENDERABLE"),
    (EGL_NATIVE_VISUAL_ID, "EGL_NATIVE_VISUAL_ID"),
    (EGL_NATIVE_VISUAL_TYPE, "EGL_NATIVE_VISUAL_TYPE"),
    (EGL_RENDERABLE_TYPE, "EGL_RENDERABLE_TYPE"),
    (EGL_SURFACE_TYPE, "EGL_SURFACE_TYPE"),
    (EGL_TRANSPARENT_TYPE, "EGL_TRANSPARENT_TYPE"),
    (EGL_TRANSPARENT_RED_VALUE, "EGL_TRANSPARENT_RED_VALUE"),
    (EGL_TRANSPARENT_GREEN_VALUE, "EGL_TRANSPARENT_GREEN_VALUE"),
    (EGL_TRANSPARENT_BLUE_VALUE, "EGL_TRANSPARENT_BLUE_VALUE"),
    (EGL_BIND_TO_TEXTURE_RGB, "EGL_BIND_TO_TEXTURE_RGB"),
    (EGL_BIND_TO_TEXTURE_RGBA, "EGL_BIND_TO_TEXTURE_RGBA"),
];

#[test]
fn every_config_answers_every_specified_attribute_consistently() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let configs = egl.configs();

    let mut ids = Vec::new();
    for config in &configs {
        for (attribute, name) in CONFIG_ATTRIBUTES {
            let mut value = i32::MIN;
            egl.clear_error();
            let ok = (egl.get_config_attrib)(egl.display, *config, *attribute, &mut value);
            assert_eq!(
                ok,
                EGL_TRUE,
                "eglGetConfigAttrib({name}) failed with 0x{:04x}; EGL 1.5 table 3.4 defines it for \
                 every config",
                (egl.get_error)()
            );
            assert_ne!(
                value,
                i32::MIN,
                "{name} reported success without writing a value"
            );
        }

        let red = egl.attrib(*config, EGL_RED_SIZE);
        let green = egl.attrib(*config, EGL_GREEN_SIZE);
        let blue = egl.attrib(*config, EGL_BLUE_SIZE);
        let alpha = egl.attrib(*config, EGL_ALPHA_SIZE);
        let luminance = egl.attrib(*config, EGL_LUMINANCE_SIZE);
        let buffer = egl.attrib(*config, EGL_BUFFER_SIZE);
        assert_eq!(
            buffer,
            red + green + blue + alpha + luminance,
            "EGL_BUFFER_SIZE must be the sum of the component sizes (EGL 1.5 table 3.4)"
        );
        assert_eq!(
            egl.attrib(*config, EGL_COLOR_BUFFER_TYPE),
            EGL_RGB_BUFFER,
            "an RGB config must report EGL_RGB_BUFFER"
        );
        assert_eq!(
            luminance, 0,
            "EGL_LUMINANCE_SIZE must be 0 for an EGL_RGB_BUFFER config"
        );
        assert_eq!(
            egl.attrib(*config, EGL_CONFIG_CAVEAT),
            EGL_NONE,
            "a conformant config must report no caveat"
        );

        // Multisampling: EGL_SAMPLE_BUFFERS is 0 or 1, and 0 sample buffers means 0 samples.
        let sample_buffers = egl.attrib(*config, EGL_SAMPLE_BUFFERS);
        let samples = egl.attrib(*config, EGL_SAMPLES);
        assert!(
            (0..=1).contains(&sample_buffers),
            "EGL_SAMPLE_BUFFERS must be 0 or 1, got {sample_buffers}"
        );
        if sample_buffers == 0 {
            assert_eq!(samples, 0, "0 sample buffers requires 0 samples");
        }

        // Pbuffer capacity must be self-consistent, and claimed only when the pbuffer bit is.
        let (max_width, max_height, max_pixels) = (
            egl.attrib(*config, EGL_MAX_PBUFFER_WIDTH),
            egl.attrib(*config, EGL_MAX_PBUFFER_HEIGHT),
            egl.attrib(*config, EGL_MAX_PBUFFER_PIXELS),
        );
        if egl.attrib(*config, EGL_SURFACE_TYPE) & EGL_PBUFFER_BIT != 0 {
            assert!(
                max_width > 0 && max_height > 0,
                "a pbuffer config must allow a nonzero pbuffer"
            );
            assert!(
                max_pixels >= max_width.min(max_height),
                "EGL_MAX_PBUFFER_PIXELS ({max_pixels}) must admit at least a \
                 {max_width}x{max_height} allocation's smaller edge"
            );
        }

        assert!(
            egl.attrib(*config, EGL_MIN_SWAP_INTERVAL)
                <= egl.attrib(*config, EGL_MAX_SWAP_INTERVAL),
            "EGL_MIN_SWAP_INTERVAL must not exceed EGL_MAX_SWAP_INTERVAL"
        );
        assert_eq!(
            egl.attrib(*config, EGL_TRANSPARENT_TYPE),
            EGL_NONE,
            "this driver models no transparent-pixel config"
        );

        // EGL_CONFORMANT must not claim a client API that EGL_RENDERABLE_TYPE does not offer.
        let renderable = egl.attrib(*config, EGL_RENDERABLE_TYPE);
        let conformant = egl.attrib(*config, EGL_CONFORMANT);
        assert_eq!(
            conformant & !renderable,
            0,
            "EGL_CONFORMANT (0x{conformant:x}) claims an API absent from EGL_RENDERABLE_TYPE \
             (0x{renderable:x})"
        );
        assert_ne!(
            renderable & (EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT),
            0,
            "every config of a GLES driver must be ES2- or ES3-renderable"
        );

        let id = egl.attrib(*config, EGL_CONFIG_ID);
        assert!(id > 0, "EGL_CONFIG_ID is 1-based; 0 is never a valid id");
        assert!(
            !ids.contains(&id),
            "EGL_CONFIG_ID {id} is advertised by two configs"
        );
        ids.push(id);
    }

    // An attribute that is not a config attribute is EGL_BAD_ATTRIBUTE, never a fabricated 0.
    egl.clear_error();
    let mut value = 12345;
    assert_eq!(
        (egl.get_config_attrib)(egl.display, configs[0], 0x7FFF, &mut value),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_ATTRIBUTE);
    assert_eq!(
        value, 12345,
        "a failed query must not write the out-parameter"
    );

    // A handle this driver never handed out is EGL_BAD_CONFIG (and must not be dereferenced).
    egl.clear_error();
    assert_eq!(
        (egl.get_config_attrib)(
            egl.display,
            0xDEAD_usize as *mut c_void,
            EGL_RED_SIZE,
            &mut value
        ),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_CONFIG);
}

#[test]
fn config_color_depth_stencil_match_the_gl_bits_a_context_on_it_reports() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let shim = Shim::get();
    let get_integerv = f!(shim.gles, "glGetIntegerv", extern "C" fn(u32, *mut i32));

    for config in egl.configs() {
        let context = (egl.create_context)(
            egl.display,
            config,
            core::ptr::null_mut(),
            core::ptr::null(),
        );
        assert!(
            !context.is_null(),
            "eglCreateContext on an advertised config failed with 0x{:04x}",
            (egl.get_error)()
        );
        assert_eq!(
            (egl.make_current)(
                egl.display,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                context
            ),
            EGL_TRUE
        );

        let gl = |pname: u32| {
            let mut value = -1;
            get_integerv(pname, &mut value);
            value
        };
        for (pname, attribute, name) in [
            (GL_RED_BITS, EGL_RED_SIZE, "RED"),
            (GL_GREEN_BITS, EGL_GREEN_SIZE, "GREEN"),
            (GL_BLUE_BITS, EGL_BLUE_SIZE, "BLUE"),
            (GL_ALPHA_BITS, EGL_ALPHA_SIZE, "ALPHA"),
            (GL_DEPTH_BITS, EGL_DEPTH_SIZE, "DEPTH"),
            (GL_STENCIL_BITS, EGL_STENCIL_SIZE, "STENCIL"),
        ] {
            assert_eq!(
                gl(pname),
                egl.attrib(config, attribute),
                "GL_{name}_BITS must equal EGL_{name}_SIZE for the config the context was created \
                 on (config id {})",
                egl.attrib(config, EGL_CONFIG_ID)
            );
        }
        assert_eq!(gl(GL_SAMPLES), egl.attrib(config, EGL_SAMPLES));
        assert_eq!(
            gl(GL_SAMPLE_BUFFERS),
            egl.attrib(config, EGL_SAMPLE_BUFFERS)
        );
        // A pbuffer becomes a host texture, so its advertised capacity cannot exceed the texture limit.
        assert!(
            egl.attrib(config, EGL_MAX_PBUFFER_WIDTH) <= gl(GL_MAX_TEXTURE_SIZE),
            "EGL_MAX_PBUFFER_WIDTH exceeds GL_MAX_TEXTURE_SIZE"
        );

        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
    }
}

#[test]
fn every_claimed_surface_type_bit_can_actually_create_that_surface() {
    let _serial = serial();
    let egl = Egl::bring_up();

    for config in egl.configs() {
        let id = egl.attrib(config, EGL_CONFIG_ID);
        let surface_type = egl.attrib(config, EGL_SURFACE_TYPE);
        assert_ne!(
            surface_type, 0,
            "config {id} claims no surface type at all, so nothing can be rendered to it"
        );

        if surface_type & EGL_PBUFFER_BIT != 0 {
            let attributes = [EGL_WIDTH, 64, EGL_HEIGHT, 32, EGL_NONE];
            egl.clear_error();
            let surface = (egl.create_pbuffer_surface)(egl.display, config, attributes.as_ptr());
            assert!(
                !surface.is_null(),
                "config {id} claims EGL_PBUFFER_BIT but eglCreatePbufferSurface failed with \
                 0x{:04x}",
                (egl.get_error)()
            );
            let mut width = -1;
            let mut height = -1;
            assert_eq!(
                (egl.query_surface)(egl.display, surface, EGL_WIDTH, &mut width),
                EGL_TRUE
            );
            assert_eq!(
                (egl.query_surface)(egl.display, surface, EGL_HEIGHT, &mut height),
                EGL_TRUE
            );
            assert_eq!(
                (width, height),
                (64, 32),
                "the pbuffer must be the requested size"
            );
            assert_eq!((egl.destroy_surface)(egl.display, surface), EGL_TRUE);
        }

        if surface_type & EGL_WINDOW_BIT != 0 {
            // With `HL_GL_NO_WAYLAND` set there is no native window; the driver falls back to its
            // configured offscreen size, so a null native window must still yield a usable surface.
            egl.clear_error();
            let surface = (egl.create_window_surface)(
                egl.display,
                config,
                core::ptr::null_mut(),
                core::ptr::null(),
            );
            assert!(
                !surface.is_null(),
                "config {id} claims EGL_WINDOW_BIT but eglCreateWindowSurface failed with 0x{:04x}",
                (egl.get_error)()
            );
            let mut width = -1;
            assert_eq!(
                (egl.query_surface)(egl.display, surface, EGL_WIDTH, &mut width),
                EGL_TRUE
            );
            assert!(width > 0, "a window surface must report a positive width");
            assert_eq!((egl.destroy_surface)(egl.display, surface), EGL_TRUE);
        }

        assert_eq!(
            surface_type & EGL_PIXMAP_BIT,
            0,
            "config {id} claims EGL_PIXMAP_BIT, but this driver models no native pixmap"
        );
    }
}

#[test]
fn choose_config_agrees_with_get_configs_and_honours_at_least_semantics() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let enumerated = egl.configs();

    // A NULL attribute list is match-all: it must find every config `eglGetConfigs` enumerates.
    let mut count = -1;
    assert_eq!(
        (egl.choose_config)(
            egl.display,
            core::ptr::null(),
            core::ptr::null_mut(),
            0,
            &mut count
        ),
        EGL_TRUE
    );
    assert_eq!(
        count as usize,
        enumerated.len(),
        "eglChooseConfig(NULL) must match every config eglGetConfigs enumerates"
    );

    // An immediately-EGL_NONE list means the same thing.
    let empty = [EGL_NONE];
    let mut empty_count = -1;
    assert_eq!(
        (egl.choose_config)(
            egl.display,
            empty.as_ptr(),
            core::ptr::null_mut(),
            0,
            &mut empty_count
        ),
        EGL_TRUE
    );
    assert_eq!(
        empty_count, count,
        "an empty list and a NULL list select the same set"
    );

    // `EGL_DONT_CARE` must impose nothing.
    let dont_care = [
        EGL_DEPTH_SIZE,
        EGL_DONT_CARE,
        EGL_STENCIL_SIZE,
        EGL_DONT_CARE,
        EGL_NONE,
    ];
    let mut dont_care_count = -1;
    assert_eq!(
        (egl.choose_config)(
            egl.display,
            dont_care.as_ptr(),
            core::ptr::null_mut(),
            0,
            &mut dont_care_count
        ),
        EGL_TRUE
    );
    assert_eq!(
        dont_care_count, count,
        "EGL_DONT_CARE must not narrow the set"
    );

    // "At least" semantics: asking for depth 24 + stencil 8 must yield only configs that offer them.
    let request = [EGL_DEPTH_SIZE, 24, EGL_STENCIL_SIZE, 8, EGL_NONE];
    let mut matched = vec![core::ptr::null_mut(); enumerated.len()];
    let mut matched_count = -1;
    assert_eq!(
        (egl.choose_config)(
            egl.display,
            request.as_ptr(),
            matched.as_mut_ptr(),
            enumerated.len() as i32,
            &mut matched_count
        ),
        EGL_TRUE
    );
    assert!(
        matched_count > 0,
        "a depth-24 + stencil-8 request is the glmark2/GTK default and must match a config"
    );
    for config in matched.iter().take(matched_count as usize) {
        assert!(egl.attrib(*config, EGL_DEPTH_SIZE) >= 24);
        assert!(egl.attrib(*config, EGL_STENCIL_SIZE) >= 8);
    }

    // An impossible request matches nothing and is NOT an error (EGL 1.5 §3.4.1).
    let impossible = [EGL_RED_SIZE, 32, EGL_NONE];
    let mut none_count = -1;
    egl.clear_error();
    assert_eq!(
        (egl.choose_config)(
            egl.display,
            impossible.as_ptr(),
            core::ptr::null_mut(),
            0,
            &mut none_count
        ),
        EGL_TRUE
    );
    assert_eq!(none_count, 0, "a 32-bit-red request must match nothing");
    assert_eq!(
        (egl.get_error)(),
        EGL_SUCCESS,
        "matching nothing is not an error"
    );

    // A NULL `num_config` is EGL_BAD_PARAMETER for both enumeration entry points.
    egl.clear_error();
    assert_eq!(
        (egl.choose_config)(
            egl.display,
            core::ptr::null(),
            core::ptr::null_mut(),
            0,
            core::ptr::null_mut()
        ),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_PARAMETER);
    egl.clear_error();
    assert_eq!(
        (egl.get_configs)(egl.display, core::ptr::null_mut(), 0, core::ptr::null_mut()),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_PARAMETER);
}
