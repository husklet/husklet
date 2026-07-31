//! The host cursor: [`HostCursor`] rendered as an `NSCursor`.
//!
//! Two different requests, two different treatments. A themed `wp_cursor_shape` name is satisfied from
//! the SYSTEM cursor set (`NSCursor`'s class cursors) so the pointer matches every other Mac app; a
//! client-provided `wl_pointer.set_cursor` surface is its own bitmap and is turned into an `NSCursor`
//! built from those exact pixels at the client's hotspot.
//!
//! Two decisions worth stating, because both are visible:
//!
//! * A shape macOS has no cursor for falls back to the ARROW, never to an approximation on the wrong
//!   axis. AppKit publishes no diagonal resize, busy/progress, help, zoom or cell cursor (the diagonal
//!   ones exist only as private `_window*` selectors, which this crate will not call). Showing a
//!   horizontal two-way arrow for `nwse-resize` claims the wrong drag direction; the arrow merely says
//!   nothing. Each unmapped shape is logged so the gap is attributable.
//! * `NSCursor` is set imperatively and AppKit re-resolves the cursor whenever the pointer crosses a
//!   window, so a request that arrives while the pointer is over ANOTHER application's window cannot be
//!   applied then — the cursor is not ours to set. The last request is therefore retained and re-applied
//!   from the pointer-motion path, which only runs for events belonging to one of our windows. The
//!   effect is that the cursor becomes correct as soon as the pointer is over guest content, and the host
//!   keeps its own cursor everywhere else.

use objc2::rc::Retained;
use objc2::ClassType;
use objc2_app_kit::{NSBitmapImageRep, NSCursor, NSDeviceRGBColorSpace, NSImage};
use objc2_foundation::{NSPoint, NSSize};

use crate::scene::port::{CursorImage, CursorShape, HostCursor};

/// Client cursor images the host could not use, counted per requested size.
static UNUSABLE_CURSOR: crate::diagnostic::SharedTally<(u32, u32)> =
    crate::diagnostic::SharedTally::new();

/// The macOS system cursors a themed shape can resolve to — everything `NSCursor` publishes that is
/// useful for a pointer shape, plus [`SystemCursor::Arrow`] as the honest fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SystemCursor {
    Arrow,
    IBeam,
    IBeamVertical,
    PointingHand,
    OpenHand,
    ClosedHand,
    Crosshair,
    ContextualMenu,
    DragLink,
    DragCopy,
    OperationNotAllowed,
    DisappearingItem,
    ResizeLeft,
    ResizeRight,
    ResizeLeftRight,
    ResizeUp,
    ResizeDown,
    ResizeUpDown,
}

impl SystemCursor {
    /// Whether this is the fallback rather than a real match for what was asked.
    fn is_fallback(self) -> bool {
        self == SystemCursor::Arrow
    }
}

/// The themed-shape → system-cursor table. Total, so no shape can be silently forgotten.
pub(super) fn system_cursor(shape: CursorShape) -> SystemCursor {
    match shape {
        CursorShape::Default => SystemCursor::Arrow,
        CursorShape::ContextMenu => SystemCursor::ContextualMenu,
        CursorShape::Pointer => SystemCursor::PointingHand,
        CursorShape::Text => SystemCursor::IBeam,
        CursorShape::VerticalText => SystemCursor::IBeamVertical,
        CursorShape::Crosshair | CursorShape::Cell => SystemCursor::Crosshair,
        CursorShape::Alias => SystemCursor::DragLink,
        CursorShape::Copy => SystemCursor::DragCopy,
        // Dragging content: macOS shows the closed hand that is holding it.
        CursorShape::Move | CursorShape::Grabbing => SystemCursor::ClosedHand,
        CursorShape::Grab | CursorShape::AllScroll => SystemCursor::OpenHand,
        CursorShape::NotAllowed => SystemCursor::OperationNotAllowed,
        // `no-drop` is "this drag will not be accepted here" — AppKit's poof cursor is the drag-specific
        // refusal, distinct from the general "forbidden" slash.
        CursorShape::NoDrop => SystemCursor::DisappearingItem,
        CursorShape::WResize => SystemCursor::ResizeLeft,
        CursorShape::EResize => SystemCursor::ResizeRight,
        CursorShape::NResize => SystemCursor::ResizeUp,
        CursorShape::SResize => SystemCursor::ResizeDown,
        CursorShape::EwResize | CursorShape::ColResize => SystemCursor::ResizeLeftRight,
        CursorShape::NsResize | CursorShape::RowResize => SystemCursor::ResizeUpDown,
        // No documented AppKit cursor: diagonal resize, busy, help, zoom. See the module note.
        CursorShape::NeResize
        | CursorShape::NwResize
        | CursorShape::SeResize
        | CursorShape::SwResize
        | CursorShape::NeswResize
        | CursorShape::NwseResize
        | CursorShape::Progress
        | CursorShape::Wait
        | CursorShape::Help
        | CursorShape::ZoomIn
        | CursorShape::ZoomOut => SystemCursor::Arrow,
    }
}

fn class_cursor(cursor: SystemCursor) -> Retained<NSCursor> {
    match cursor {
        SystemCursor::Arrow => NSCursor::arrowCursor(),
        SystemCursor::IBeam => NSCursor::IBeamCursor(),
        SystemCursor::IBeamVertical => NSCursor::IBeamCursorForVerticalLayout(),
        SystemCursor::PointingHand => NSCursor::pointingHandCursor(),
        SystemCursor::OpenHand => NSCursor::openHandCursor(),
        SystemCursor::ClosedHand => NSCursor::closedHandCursor(),
        SystemCursor::Crosshair => NSCursor::crosshairCursor(),
        SystemCursor::ContextualMenu => NSCursor::contextualMenuCursor(),
        SystemCursor::DragLink => NSCursor::dragLinkCursor(),
        SystemCursor::DragCopy => NSCursor::dragCopyCursor(),
        SystemCursor::OperationNotAllowed => NSCursor::operationNotAllowedCursor(),
        SystemCursor::DisappearingItem => NSCursor::disappearingItemCursor(),
        SystemCursor::ResizeLeft => NSCursor::resizeLeftCursor(),
        SystemCursor::ResizeRight => NSCursor::resizeRightCursor(),
        SystemCursor::ResizeLeftRight => NSCursor::resizeLeftRightCursor(),
        SystemCursor::ResizeUp => NSCursor::resizeUpCursor(),
        SystemCursor::ResizeDown => NSCursor::resizeDownCursor(),
        SystemCursor::ResizeUpDown => NSCursor::resizeUpDownCursor(),
    }
}

/// Logical (point) size and hotspot of a client cursor image at its buffer scale. A 2× cursor buffer is
/// half its pixel size on screen, and `NSImage` is measured in points, so the scale belongs here.
pub(super) fn image_geometry(image: &CursorImage) -> ((f64, f64), (f64, f64)) {
    let scale = f64::from(image.scale.max(1));
    let size = (
        f64::from(image.width) / scale,
        f64::from(image.height) / scale,
    );
    // The hotspot is already in surface-local logical coordinates, clamped inside the image because
    // AppKit rejects a hot spot outside it.
    let hotspot = (
        f64::from(image.hotspot.0).clamp(0.0, size.0),
        f64::from(image.hotspot.1).clamp(0.0, size.1),
    );
    (size, hotspot)
}

/// Build an `NSCursor` from a client's cursor surface pixels (premultiplied RGBA).
fn image_cursor(image: &CursorImage) -> Option<Retained<NSCursor>> {
    let width = isize::try_from(image.width).ok()?;
    let height = isize::try_from(image.height).ok()?;
    let stride = width.checked_mul(4)?;
    if image.rgba.len() != (stride as usize).checked_mul(height as usize)? {
        return None;
    }
    // A NULL plane makes AppKit own (and free) the pixel storage; handing it our Vec's pointer instead
    // would leave it reading freed memory the moment the request is superseded.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width,
            height,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            stride,
            32,
        )
    }?;
    let destination = unsafe { rep.bitmapData() };
    if destination.is_null() {
        return None;
    }
    let row_bytes = unsafe { rep.bytesPerRow() };
    if row_bytes < stride {
        return None;
    }
    for row in 0..height {
        let source = &image.rgba[(row * stride) as usize..((row + 1) * stride) as usize];
        // SAFETY: `bitmapData` is AppKit's own `height * bytesPerRow` buffer for this representation,
        // `row < height`, and `stride <= bytesPerRow`, so the destination row is fully in bounds.
        unsafe {
            std::ptr::copy_nonoverlapping(
                source.as_ptr(),
                destination.add((row * row_bytes) as usize),
                stride as usize,
            );
        }
    }
    let ((point_width, point_height), hotspot) = image_geometry(image);
    let ns_image =
        unsafe { NSImage::initWithSize(NSImage::alloc(), NSSize::new(point_width, point_height)) };
    unsafe { ns_image.addRepresentation(&rep) };
    Some(NSCursor::initWithImage_hotSpot(
        NSCursor::alloc(),
        &ns_image,
        NSPoint::new(hotspot.0, hotspot.1),
    ))
}

/// The host cursor the compositor last asked for, and whether AppKit's cursor is currently hidden.
///
/// `NSCursor::hide`/`unhide` are a BALANCED counter — an unmatched call leaves the pointer invisible for
/// the whole session — so the hidden state is tracked rather than re-asserted.
#[derive(Default)]
pub(super) struct HostCursorState {
    desired: Option<Retained<NSCursor>>,
    hidden: bool,
}

impl HostCursorState {
    /// Adopt a new request and apply it if the pointer is over our content.
    pub(super) fn request(&mut self, cursor: &HostCursor) {
        match cursor {
            HostCursor::Hidden => {
                self.desired = None;
                if !self.hidden {
                    self.hidden = true;
                    unsafe { NSCursor::hide() };
                }
                return;
            }
            HostCursor::Shape(shape) => {
                let system = system_cursor(*shape);
                if system.is_fallback() && *shape != CursorShape::Default {
                    hl_log::hl_log!(
                        hl_log::tag::PRESENT,
                        hl_log::Level::Debug,
                        "cursor shape '{}' has no AppKit equivalent; showing the arrow",
                        shape.css_name()
                    );
                }
                self.desired = Some(class_cursor(system));
            }
            HostCursor::Image(image) => match image_cursor(image) {
                Some(cursor) => self.desired = Some(cursor),
                None => {
                    // The client asked for a cursor and silently did not get it. Visible to the user as
                    // a wrong pointer, with nothing on record at `warn` in a release build.
                    if let Some(count) = UNUSABLE_CURSOR.record((image.width, image.height)) {
                        hl_log::hl_error!(
                            hl_log::tag::PRESENT,
                            "client cursor image {}x{} is unusable count={count}; keeping the previous \
                             cursor",
                            image.width,
                            image.height
                        );
                    }
                    return;
                }
            },
        }
        if self.hidden {
            self.hidden = false;
            unsafe { NSCursor::unhide() };
        }
        self.apply();
    }

    /// Re-assert the requested cursor. AppKit resolves the cursor afresh as the pointer moves over a view
    /// that owns no cursor rect, so the presenter calls this from the motion path for its own windows.
    pub(super) fn apply(&self) {
        if let Some(cursor) = &self.desired {
            unsafe { cursor.set() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_themed_shape_resolves_to_a_system_cursor() {
        // A missing arm would be a compile error; this pins the ones that must NOT be the fallback,
        // because those are the shapes a user notices: text entry, links, split handles, drags.
        let mapped = [
            (CursorShape::Text, SystemCursor::IBeam),
            (CursorShape::VerticalText, SystemCursor::IBeamVertical),
            (CursorShape::Pointer, SystemCursor::PointingHand),
            (CursorShape::Grab, SystemCursor::OpenHand),
            (CursorShape::Grabbing, SystemCursor::ClosedHand),
            (CursorShape::EwResize, SystemCursor::ResizeLeftRight),
            (CursorShape::NsResize, SystemCursor::ResizeUpDown),
            (CursorShape::ColResize, SystemCursor::ResizeLeftRight),
            (CursorShape::RowResize, SystemCursor::ResizeUpDown),
            (CursorShape::EResize, SystemCursor::ResizeRight),
            (CursorShape::WResize, SystemCursor::ResizeLeft),
            (CursorShape::NResize, SystemCursor::ResizeUp),
            (CursorShape::SResize, SystemCursor::ResizeDown),
            (CursorShape::NotAllowed, SystemCursor::OperationNotAllowed),
            (CursorShape::ContextMenu, SystemCursor::ContextualMenu),
            (CursorShape::Copy, SystemCursor::DragCopy),
            (CursorShape::Alias, SystemCursor::DragLink),
            (CursorShape::Crosshair, SystemCursor::Crosshair),
        ];
        for (shape, expected) in mapped {
            assert_eq!(system_cursor(shape), expected, "shape {shape:?}");
        }
    }

    #[test]
    fn only_the_shapes_appkit_lacks_fall_back_to_the_arrow() {
        let expected_fallbacks = [
            CursorShape::Default,
            CursorShape::Help,
            CursorShape::Progress,
            CursorShape::Wait,
            CursorShape::NeResize,
            CursorShape::NwResize,
            CursorShape::SeResize,
            CursorShape::SwResize,
            CursorShape::NeswResize,
            CursorShape::NwseResize,
            CursorShape::ZoomIn,
            CursorShape::ZoomOut,
        ];
        let actual = CursorShape::all()
            .filter(|shape| system_cursor(*shape).is_fallback())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected_fallbacks);
    }

    #[test]
    fn a_scaled_cursor_image_is_measured_in_points_and_its_hotspot_stays_inside() {
        let image = CursorImage {
            rgba: vec![0; 48 * 48 * 4],
            width: 48,
            height: 48,
            scale: 2,
            hotspot: (6, 7),
        };
        assert_eq!(image_geometry(&image), ((24.0, 24.0), (6.0, 7.0)));

        let outside = CursorImage {
            hotspot: (400, -3),
            ..image
        };
        assert_eq!(image_geometry(&outside), ((24.0, 24.0), (24.0, 0.0)));
    }
}
