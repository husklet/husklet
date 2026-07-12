//! `wl_compositor` / `wl_subcompositor` / `wl_shm` handlers and the commit → present path.
//!
//! `commit()` snapshots the committed surface state (buffer, viewport, frame + presentation-feedback
//! callbacks), repacks the `wl_shm` pixels into dd-display's tight-BGRA [`SurfaceBuffer`], hands it to
//! the boxed [`Presenter`], fires the frame callbacks so the client keeps drawing, and answers
//! `wp_presentation` feedback so Chrome/viz's BeginFrameSource keeps ticking. This is the exact seam
//! `server.rs` drives; the difference is Smithay decoded the wire for us.

use std::time::Duration;

use dd_display::present::SurfaceBuffer;

use smithay::{
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
            Client,
        },
    },
    utils::Size,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            with_states, BufferAssignment, CompositorClientState, CompositorHandler,
            CompositorState, SurfaceAttributes,
        },
        presentation::{PresentationFeedbackCachedState, Refresh},
        shm::{with_buffer_contents, ShmHandler, ShmState},
        viewporter::ViewportCachedState,
    },
};

use crate::{surface_id, ClientState, DdState, OUTPUT_REFRESH_MHZ};

impl CompositorHandler for DdState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }
    fn commit(&mut self, surface: &WlSurface) {
        self.present_surface(surface);
    }
}

impl BufferHandler for DdState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for DdState {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl DdState {
    /// The commit → present path: pull the committed `wl_shm` buffer, repack it tight-BGRA into a
    /// [`SurfaceBuffer`], hand it to the Presenter (which opens/updates the NSWindow on macOS), fire
    /// the surface's frame callbacks so the client keeps drawing, and answer any `wp_presentation`
    /// feedback so Chrome/viz's BeginFrameSource keeps its frame clock ticking.
    pub(crate) fn present_surface(&mut self, surface: &WlSurface) {
        let sid = surface_id(surface);

        // Snapshot the committed state. The three cached-state guards must not overlap, so scope each.
        let (buffer, buffer_scale, callbacks, dst, src, feedback) = with_states(surface, |states| {
            let (buffer, scale, callbacks) = {
                let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                let cur = attrs.current();
                let buffer = match &cur.buffer {
                    Some(BufferAssignment::NewBuffer(b)) => Some(b.clone()),
                    _ => None,
                };
                let callbacks: Vec<_> = std::mem::take(&mut cur.frame_callbacks);
                (buffer, cur.buffer_scale.max(1), callbacks)
            };
            let (dst, src) = {
                let mut vp = states.cached_state.get::<ViewportCachedState>();
                let cur = vp.current();
                // wp_viewport source: a crop rectangle in post-buffer-scale (logical) surface coords.
                let src = cur.src.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h));
                (cur.size(), src)
            };
            // wp_presentation_feedback callbacks committed for THIS content update: drain current()
            // before presenting, answer after.
            let feedback = std::mem::take(
                &mut states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks,
            );
            (buffer, scale, callbacks, dst, src, feedback)
        });

        let Some(buffer) = buffer else {
            // No new buffer this commit (e.g. the initial role commit) — still ack frame callbacks and
            // discard the feedback (there is no content update to time).
            let t = self.now_ms();
            for cb in callbacks {
                cb.done(t);
            }
            for fb in feedback {
                fb.discarded();
            }
            return;
        };

        // Present is non-blocking (the Metal path never blocks on nextDrawable — see present_cocoa); a
        // failed present (false) must NOT advance frame pacing, exactly as `server.rs` gates it.
        let did_present = match self.build_surface_buffer(sid, &buffer, buffer_scale, dst, src) {
            Some(surf) => self.presenter.present(&surf),
            None => false,
        };

        // Frame callbacks: without these the client stops after one frame.
        let t = self.now_ms();
        for cb in callbacks {
            cb.done(t);
        }

        // Answer presentation feedback. Presented ⇒ sync_output + presented(monotonic ts, refresh, MSC,
        // vsync); otherwise discarded. The frame is on-screen by the time `present` returns (the analogue
        // of weston answering on the KMS pageflip-complete event).
        self.send_presentation_feedback(feedback, did_present);
    }

    /// Answer every `wp_presentation_feedback` for a just-processed commit, mirroring `server.rs`'s
    /// `send_presentation_feedback` on the Smithay callback objects.
    fn send_presentation_feedback(
        &mut self,
        feedback: Vec<smithay::wayland::presentation::PresentationFeedbackCallback>,
        did_present: bool,
    ) {
        if feedback.is_empty() {
            return;
        }
        if !did_present {
            for fb in feedback {
                fb.discarded();
            }
            return;
        }
        self.present_seq = self.present_seq.wrapping_add(1);
        let seq = self.present_seq;
        let time = monotonic_now();
        let refresh = Refresh::fixed(output_refresh());
        for fb in feedback {
            fb.presented(
                &self.output,
                time,
                refresh,
                seq,
                wp_presentation_feedback::Kind::Vsync,
            );
        }
    }

    /// Repack a committed `wl_shm` buffer into dd-display's tight-BGRA [`SurfaceBuffer`]. The backing
    /// texture is the full buffer; the logical size is the `wp_viewport` destination if set, else the
    /// buffer pixels divided by `wl_surface.buffer_scale` (so a HiDPI 2x buffer maps to logical units).
    fn build_surface_buffer(
        &self,
        sid: u32,
        buffer: &WlBuffer,
        buffer_scale: i32,
        dst: Option<Size<i32, smithay::utils::Logical>>,
        src: Option<(f64, f64, f64, f64)>,
    ) -> Option<SurfaceBuffer> {
        let title = self.titles.get(&sid).cloned().unwrap_or_else(|| "dd".into());
        let res = with_buffer_contents(buffer, |ptr, _len, data| {
            let w = data.width;
            let h = data.height;
            let stride = data.stride;
            let src_off = data.offset;
            let fmt = match data.format {
                wl_shm::Format::Xrgb8888 => 1u32, // opaque (dd-display convention: format==1 ⇒ XRGB)
                _ => 0u32,                        // ARGB8888 (and anything else): honour alpha
            };
            // Tight BGRA copy of the backing texture, honouring the pool offset + row stride.
            let tight = (w * 4) as usize;
            let mut bgra = vec![0u8; tight * h as usize];
            for row in 0..h as isize {
                let src = unsafe { ptr.offset(src_off as isize + row * stride as isize) };
                let dstart = row as usize * tight;
                unsafe {
                    std::ptr::copy_nonoverlapping(src, bgra[dstart..].as_mut_ptr(), tight);
                }
            }
            (w, h, fmt, bgra)
        })
        .ok()?;
        let (tex_w, tex_h, fmt, bgra) = res;

        // wp_viewport source rectangle (given in post-buffer-scale/logical coords) → a normalized
        // sample rect in the backing texture, so a client that renders into an oversized target and
        // crops via the viewport (Chrome's fractional-scale path) presents only the requested region.
        // `dst` (or, absent it, the buffer pixels / buffer_scale) is the on-screen logical size.
        let (log_w, log_h, uv_rect) = match (dst, src) {
            (Some(sz), src) if sz.w > 0 && sz.h > 0 => {
                (sz.w, sz.h, uv_from_src(src, tex_w, tex_h, buffer_scale))
            }
            (None, Some((_, _, sw, sh))) if sw > 0.0 && sh > 0.0 => (
                (sw.round() as i32).max(1),
                (sh.round() as i32).max(1),
                uv_from_src(src, tex_w, tex_h, buffer_scale),
            ),
            _ => (
                (tex_w / buffer_scale).max(1),
                (tex_h / buffer_scale).max(1),
                [0.0, 0.0, 1.0, 1.0],
            ),
        };

        Some(SurfaceBuffer {
            sid,
            width: log_w,
            height: log_h,
            texture_width: tex_w,
            texture_height: tex_h,
            stride: tex_w * 4,
            format: fmt,
            bgra,
            title,
            iosurface_id: None,
            gpu_render: false,
            uv_rect,
        })
    }
}

/// Normalize a `wp_viewport` source rectangle `(x, y, w, h)` — given in post-buffer-scale/logical
/// coords — into a `[u0, v0, u1, v1]` sample rect over the backing texture (buffer pixels). Returns the
/// full texture when there is no source crop or the texture has no area.
fn uv_from_src(src: Option<(f64, f64, f64, f64)>, tex_w: i32, tex_h: i32, buffer_scale: i32) -> [f32; 4] {
    match src {
        Some((x, y, w, h)) if tex_w > 0 && tex_h > 0 && w > 0.0 && h > 0.0 => {
            let s = buffer_scale.max(1) as f64;
            let (tw, th) = (tex_w as f64, tex_h as f64);
            let u0 = ((x * s) / tw).clamp(0.0, 1.0) as f32;
            let v0 = ((y * s) / th).clamp(0.0, 1.0) as f32;
            let u1 = (((x + w) * s) / tw).clamp(0.0, 1.0) as f32;
            let v1 = (((y + h) * s) / th).clamp(0.0, 1.0) as f32;
            [u0, v0, u1, v1]
        }
        _ => [0.0, 0.0, 1.0, 1.0],
    }
}

/// Host `CLOCK_MONOTONIC` as a `Duration` (the `wp_presentation.presented` timestamp). dd runs the guest
/// clock in the host's monotonic domain, so this is the value the guest reads back — mirrors
/// `server.rs`'s `monotonic_now`.
fn monotonic_now() -> Duration {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    Duration::new(ts.tv_sec as u64, (ts.tv_nsec as u32) % 1_000_000_000)
}

/// The output refresh interval (time until the next vblank) derived from the advertised mode. 60000 mHz
/// ⇒ ~16.667 ms; clients add multiples of it to predict future vblanks.
fn output_refresh() -> Duration {
    if OUTPUT_REFRESH_MHZ <= 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(1_000_000_000_000u64 / OUTPUT_REFRESH_MHZ as u64)
}
