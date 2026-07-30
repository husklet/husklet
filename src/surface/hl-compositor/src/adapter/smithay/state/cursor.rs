//! Turning a client's cursor requests into a host cursor.
//!
//! `wl_pointer.set_cursor` and the cursor surface's own `wl_surface.commit` are two independent events in
//! either order: the request carries the hotspot and names the surface, the commit carries the pixels.
//! This module keeps both halves and hands the port a [`HostCursor::Image`] as soon as they agree.

use super::*;
use crate::scene::port::{CursorImage, HostCursor};

impl HlState {
    /// Retain `stored` as the cursor image if `sid` is the surface a client set as its pointer cursor,
    /// then publish. Called from the commit path with the pixels it is about to deposit — a cursor surface
    /// never presents as a window, so this is the only place those pixels are used.
    pub(super) fn stash_cursor_pixels(
        &mut self,
        sid: SurfaceId,
        stored: &StoredBuffer,
        scale: i32,
    ) {
        let is_cursor = self.cursor_surface.is_some_and(|(cursor, _)| cursor == sid)
            || self
                .engine
                .scene
                .get(sid)
                .is_some_and(|surface| matches!(surface.role, SurfaceRole::Cursor));
        if !is_cursor {
            return;
        }
        let Ok(width) = u32::try_from(stored.width) else {
            return;
        };
        let Ok(height) = u32::try_from(stored.height) else {
            return;
        };
        if width == 0 || height == 0 || stored.rgba.len() != (width as usize * height as usize * 4)
        {
            return;
        }
        // The neutral image is premultiplied RGBA; the macOS deposit path keeps native BGRA order, so
        // canonicalize here rather than teaching every backend both orders for a 24×24 buffer.
        let mut rgba = stored.rgba.clone();
        if stored.bgra {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        self.cursor_pixels = Some((
            sid,
            CursorImage {
                rgba,
                width,
                height,
                scale: scale.max(1),
                hotspot: (0, 0),
            },
        ));
        self.publish_host_cursor();
    }

    /// Hand the host the cursor image once both the `set_cursor` request (surface + hotspot) and that
    /// surface's committed pixels are present. Either alone is not yet a drawable cursor.
    pub(super) fn publish_host_cursor(&mut self) {
        let Some((surface, hotspot)) = self.cursor_surface else {
            return;
        };
        let Some(image) = self
            .cursor_pixels
            .as_ref()
            .filter(|(sid, _)| *sid == surface)
            .map(|(_, image)| image.clone())
        else {
            return;
        };
        hl_debug!(
            tag::WAYLAND,
            "wl_pointer.set_cursor -> host cursor image {}x{} scale={} hotspot={},{}",
            image.width,
            image.height,
            image.scale,
            hotspot.0,
            hotspot.1
        );
        self.engine
            .presenter_mut()
            .set_cursor(&HostCursor::Image(CursorImage { hotspot, ..image }));
    }

    /// A cursor surface whose buffer was detached draws nothing — `wl_surface.attach(NULL)` on the cursor
    /// surface is how a client unmaps its cursor image, and leaving the previous pixels on screen would
    /// keep showing a cursor the client withdrew.
    pub(super) fn drop_cursor_pixels(&mut self, sid: SurfaceId) {
        if self
            .cursor_pixels
            .as_ref()
            .is_some_and(|(cursor, _)| *cursor == sid)
        {
            self.cursor_pixels = None;
            if self.cursor_surface.is_some_and(|(cursor, _)| cursor == sid) {
                self.engine.presenter_mut().set_cursor(&HostCursor::Hidden);
            }
        }
    }
}
