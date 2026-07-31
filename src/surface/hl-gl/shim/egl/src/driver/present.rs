use super::*;
use crate::state::SurfaceInfo;
use hl_gl::model::context::SurfaceKind;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

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

/// Which route each presented frame actually took, since process start.
///
/// `swap` already decides this every frame and used to discard it, which is precisely how a window that
/// silently stopped presenting zero-copy — every frame degraded to a `glReadPixels` readback onto the
/// shim's own mirror window — passed a 115-frame run, the rung-0 clients, `eglinfo` and glmark2. Correct
/// pixels by the wrong route is invisible to any check that only looks at pixels. These counters make it
/// assertable: a window client that is accelerated ends with `readback=0`.
pub(crate) struct PresentStats;

/// Window frames committed on the app's own surface with no readback (the accelerated route).
static NATIVE_FRAMES: AtomicU64 = AtomicU64::new(0);
/// Window frames that went through a `glReadPixels` readback because the app-surface presenter had no
/// native frame to reserve. On a window surface this is always a degradation, never a normal route.
static READBACK_FRAMES: AtomicU64 = AtomicU64::new(0);

impl PresentStats {
    pub(crate) fn record_native() {
        NATIVE_FRAMES.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a degraded window frame, and say so ONCE per process at `error` — the one level a release
    /// build keeps. Per-frame logging would drown the app; staying silent is what hid this for so long.
    pub(crate) fn record_readback(wl_surface: usize) {
        if READBACK_FRAMES.fetch_add(1, Ordering::Relaxed) != 0 {
            return;
        }
        hl_log::hl_error!(
            hl_log::tag::PRESENT,
            "window surface is presenting by glReadPixels readback, not zero-copy: no native frame \
             was reservable on wl_surface={:#x}. Pixels stay correct; acceleration does not.",
            wl_surface
        );
    }

    pub(crate) fn counts() -> (u64, u64) {
        (
            NATIVE_FRAMES.load(Ordering::Relaxed),
            READBACK_FRAMES.load(Ordering::Relaxed),
        )
    }

    /// The route a presented frame took. A window surface with no native frame reserved is the degraded
    /// readback route; anything else (pbuffer, surfaceless, a frame the submit never produced) is not
    /// this counter's business.
    fn record(prepared: &Prepared, wl_surface: usize) {
        if prepared.native.is_some() {
            Self::record_native();
        } else if prepared.app_surface {
            Self::record_readback(wl_surface);
        }
        Self::publish();
    }

    /// The destination for [`Self::publish`], from `HL_GL_COUNTERS`, resolved once. Unset means the
    /// driver writes nothing at all — a driver inside Chrome's GPU process does not touch the filesystem
    /// because it was loaded. `%p` in the path is replaced by this process's pid, which is what makes the
    /// file usable for a multi-process client where one fixed path would have every process racing it.
    fn destination() -> Option<&'static std::path::Path> {
        static PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
        PATH.get_or_init(|| {
            let requested = std::env::var("HL_GL_COUNTERS").ok()?;
            if requested.is_empty() {
                return None;
            }
            Some(std::path::PathBuf::from(
                requested.replace("%p", &std::process::id().to_string()),
            ))
        })
        .as_deref()
    }

    /// Publish the counters where a harness can read them WHILE the client is still presenting, which is
    /// the only time they can be read at all — a process that has exited has no counters and no maps.
    ///
    /// Throttled, and written through a temporary + rename so a reader never sees a half-written record.
    /// Errors are swallowed deliberately: a driver must not fail a frame because a diagnostic file could
    /// not be written.
    fn publish() {
        let Some(path) = Self::destination() else {
            return;
        };
        // ~4 writes a second is far more resolution than a reader needs and keeps this off the hot path.
        const INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
        static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
        let now = std::time::Instant::now();
        {
            let mut last = LAST.lock().unwrap_or_else(|error| error.into_inner());
            if last.is_some_and(|at| now.duration_since(at) < INTERVAL) {
                return;
            }
            *last = Some(now);
        }
        let (native, readback) = Self::counts();
        let record = format!(
            "{{\"frames_native\":{native},\"frames_readback\":{readback},\"pid\":{}}}\n",
            std::process::id()
        );
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&temporary, record).is_ok() && std::fs::rename(&temporary, path).is_err()
        {
            let _ = std::fs::remove_file(&temporary);
        }
    }
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
    // Account the route this frame actually took, now that the submit says a frame was produced. A window
    // surface with no native frame reserved is the degraded readback route — see [`PresentStats`].
    if submitted.frame {
        PresentStats::record(&prepared, info.wl_surface);
    }
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

#[cfg(test)]
mod counter_publication {
    use super::*;

    /// Body run in a FRESH process by the test below: record a native frame and publish. Ignored by
    /// default because it is only meaningful with `HL_GL_COUNTERS` set and a virgin `OnceLock`.
    #[test]
    #[ignore = "driven as a subprocess by counters_are_published_where_a_harness_can_read_them"]
    fn publish_for_subprocess() {
        PresentStats::record_native();
        PresentStats::record_native();
        PresentStats::publish();
    }

    /// The counters must be readable from OUTSIDE the process, while it is still presenting — a client
    /// that has exited has no counters and no maps, which is exactly why the acceleration gate could not
    /// answer. Unset must write nothing: the driver does not touch the filesystem because it was loaded.
    #[test]
    fn counters_are_published_where_a_harness_can_read_them() {
        let directory = std::env::temp_dir().join(format!("hl-gl-counters-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("scratch directory");

        let run = |value: Option<&std::path::Path>| {
            let mut command =
                std::process::Command::new(std::env::current_exe().expect("test binary"));
            command.args([
                "--exact",
                "--ignored",
                "--nocapture",
                "driver::present::counter_publication::publish_for_subprocess",
            ]);
            match value {
                Some(path) => command.env("HL_GL_COUNTERS", path),
                None => command.env_remove("HL_GL_COUNTERS"),
            };
            let output = command.output().expect("re-exec the test binary");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "the subprocess body must have run"
            );
        };

        // `%p` becomes the writing process's pid, so a multi-process client does not race one path.
        run(Some(&directory.join("counters-%p.json")));
        let written: Vec<_> = std::fs::read_dir(&directory)
            .expect("scratch directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        assert_eq!(
            written.len(),
            1,
            "exactly one pid-suffixed record: {written:?}"
        );
        let text = std::fs::read_to_string(&written[0]).expect("counter record");
        assert!(
            text.contains("\"frames_native\":2") && text.contains("\"frames_readback\":0"),
            "unexpected record {text:?}"
        );
        assert!(
            !written[0].to_string_lossy().contains("%p"),
            "the pid placeholder must be substituted, got {:?}",
            written[0]
        );

        // Unset: nothing new appears.
        run(None);
        let after = std::fs::read_dir(&directory).expect("scratch").count();
        assert_eq!(after, 1, "an unset HL_GL_COUNTERS must write nothing");

        let _ = std::fs::remove_dir_all(&directory);
    }
}
