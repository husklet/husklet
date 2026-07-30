use super::*;
use crate::state::SurfaceInfo;
use hl_gl::model::context::SurfaceKind;

struct Prepared {
    info: SurfaceInfo,
    app_surface: bool,
    native: Option<hl_gl::adapter::wayland_app::NativeFrame>,
    readback: bool,
}

struct Submitted {
    pixels: Option<Vec<u8>>,
    frame: bool,
}

pub(super) fn swap(display: usize, token: usize) -> u32 {
    crate::stub::trace("eglSwapBuffers", "presenting frame");
    if display != DISPLAY_TOKEN {
        GlobalState::access(|state| state.set_egl_error(EGL_BAD_DISPLAY));
        return EGL_FALSE;
    }
    if current::draw_surface() != token {
        GlobalState::access(|state| state.set_egl_error(EGL_BAD_SURFACE));
        return EGL_FALSE;
    }
    let prepared = GlobalState::access(|state| {
        if !state.has_surface(token) {
            state.set_egl_error(EGL_BAD_SURFACE);
            return None;
        }
        state.refresh_surface(token);
        let info = state.surface(token)?;
        let app_surface = info.kind == SurfaceKind::Window && info.wl_surface != 0;
        let native = app_surface.then(|| state.reserve_native_frame()).flatten();
        Some(Prepared {
            info,
            app_surface,
            native,
            readback: native.is_none() && (app_surface || state.wl.is_some()),
        })
    });
    let Some(prepared) = prepared else {
        return EGL_FALSE;
    };
    let capture = std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some();
    let info = prepared.info;
    let native_token = prepared.native.map(|frame| frame.token);
    let native_serial = prepared.native.map(|frame| frame.serial);
    let readback = prepared.readback || capture;
    let deliver_readback = prepared.readback;
    let result = if readback {
        GlobalState::gpu_io(std::time::Duration::from_secs(31), move |group, sink| {
            group
                .gl
                .bind_draw_surface(token as u64, info.render, info.kind);
            group.gl.set_present_frame(native_token, native_serial);
            let (width, height) = (info.render.width as i32, info.render.height as i32);
            readpixels::prepare_swap_xrgb(&mut group.gl, sink, width, height)
                .map(|prepared| (prepared, deliver_readback))
        })
        .map(|completed| {
            let raw = completed.observations.into_iter().find_map(|observation| {
                if let crate::state::Observation::Read(bytes) = observation {
                    Some(bytes)
                } else {
                    None
                }
            });
            let (prepared, deliver) = completed.value;
            let pixels = prepared.complete(raw);
            Submitted {
                frame: pixels.is_some(),
                pixels: deliver.then_some(pixels).flatten(),
            }
        })
    } else {
        GlobalState::gpu_submit(move |group, sink| {
            group
                .gl
                .bind_draw_surface(token as u64, info.render, info.kind);
            group.gl.set_present_frame(native_token, native_serial);
            Ok(Submitted {
                frame: swap::swap_buffers(&mut group.gl, sink)?,
                pixels: None,
            })
        })
    };
    let submitted = match result {
        Ok(submitted) => submitted,
        Err(error) => {
            GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
            return EGL_FALSE;
        }
    };
    GlobalState::access(|state| {
        if let Some(frame) = prepared.native {
            if submitted.frame
                && !state.commit_native_frame(frame, info.render.width, info.render.height)
            {
                hl_log::hl_warn!(
                    hl_log::tag::PRESENT,
                    "native surface association failed after GPU submit"
                );
            }
            return EGL_TRUE;
        }
        let Some(pixels) = submitted.pixels else {
            return EGL_TRUE;
        };
        if prepared.app_surface
            && matches!(
                state.present_to_app_surface(&pixels, info.render.width, info.render.height),
                AppPresentOutcome::Presented
            )
        {
            return EGL_TRUE;
        }
        if state.wl.is_none() && std::env::var_os("HL_GL_NO_WAYLAND").is_none() {
            let geometry =
                hl_gl::adapter::wayland::Geometry::backing(info.render.width, info.render.height);
            state.wl = hl_gl::adapter::wayland::Wayland::connect_and_handshake(&geometry);
        }
        let geometry =
            hl_gl::adapter::wayland::Geometry::backing(info.render.width, info.render.height);
        if let Some(wayland) = state.wl.as_mut() {
            if let Err(error) = wayland.commit(&pixels, &geometry) {
                hl_log::hl_warn!(
                    hl_log::tag::PRESENT,
                    "Wayland mirror commit failed after GPU present: {error:?}"
                );
            }
        }
        EGL_TRUE
    })
}
