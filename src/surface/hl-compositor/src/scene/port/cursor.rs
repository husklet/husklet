//! The host-cursor half of the [`Windows`](super::Windows) port: what the compositor asks the host to
//! draw as the pointer cursor.
//!
//! Wayland offers two mechanisms and the compositor advertises both. `wp_cursor_shape_device_v1` names a
//! THEMED shape and expects the compositor's own theme (on a host platform: the system cursor set); a
//! `wl_pointer.set_cursor` surface hands over PIXELS plus a hotspot and expects them drawn verbatim. They
//! are different kinds of request, so they are different variants here — a backend maps the first onto
//! system cursors and rasterizes the second.

/// A `wp_cursor_shape_device_v1` shape, decoded from the CSS name the protocol enum carries. The complete
/// v1 shape set: a backend must decide what each one becomes, including the ones its platform lacks.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CursorShape {
    Default,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    EResize,
    NResize,
    NeResize,
    NwResize,
    SResize,
    SeResize,
    SwResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ColResize,
    RowResize,
    AllScroll,
    ZoomIn,
    ZoomOut,
}

/// The CSS name of every shape, in wire order. `cursor-icon`'s names (what Smithay decodes `set_shape`
/// into) are exactly these, so this table is the whole decode.
const NAMES: [(&str, CursorShape); 34] = [
    ("default", CursorShape::Default),
    ("context-menu", CursorShape::ContextMenu),
    ("help", CursorShape::Help),
    ("pointer", CursorShape::Pointer),
    ("progress", CursorShape::Progress),
    ("wait", CursorShape::Wait),
    ("cell", CursorShape::Cell),
    ("crosshair", CursorShape::Crosshair),
    ("text", CursorShape::Text),
    ("vertical-text", CursorShape::VerticalText),
    ("alias", CursorShape::Alias),
    ("copy", CursorShape::Copy),
    ("move", CursorShape::Move),
    ("no-drop", CursorShape::NoDrop),
    ("not-allowed", CursorShape::NotAllowed),
    ("grab", CursorShape::Grab),
    ("grabbing", CursorShape::Grabbing),
    ("e-resize", CursorShape::EResize),
    ("n-resize", CursorShape::NResize),
    ("ne-resize", CursorShape::NeResize),
    ("nw-resize", CursorShape::NwResize),
    ("s-resize", CursorShape::SResize),
    ("se-resize", CursorShape::SeResize),
    ("sw-resize", CursorShape::SwResize),
    ("w-resize", CursorShape::WResize),
    ("ew-resize", CursorShape::EwResize),
    ("ns-resize", CursorShape::NsResize),
    ("nesw-resize", CursorShape::NeswResize),
    ("nwse-resize", CursorShape::NwseResize),
    ("col-resize", CursorShape::ColResize),
    ("row-resize", CursorShape::RowResize),
    ("all-scroll", CursorShape::AllScroll),
    ("zoom-in", CursorShape::ZoomIn),
    ("zoom-out", CursorShape::ZoomOut),
];

impl CursorShape {
    /// Decode a CSS cursor name. `None` for a name outside the `wp_cursor_shape` v1 set — the caller
    /// decides the fallback rather than this silently picking one.
    pub fn from_name(name: &str) -> Option<CursorShape> {
        NAMES
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, shape)| *shape)
    }

    pub fn css_name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(_, shape)| *shape == self)
            .map(|(name, _)| *name)
            .unwrap_or("default")
    }

    /// Every shape, so a backend's mapping table can be proved exhaustive.
    pub fn all() -> impl Iterator<Item = CursorShape> {
        NAMES.iter().map(|(_, shape)| *shape)
    }
}

/// The pixels a client attached to its `wl_pointer.set_cursor` surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorImage {
    /// Tight `width*4` rows, top-left origin, PREMULTIPLIED RGBA (`wl_shm`'s ARGB8888 canonicalized the
    /// way the neutral presenter store does it).
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// `wl_surface.set_buffer_scale`: buffer pixels per logical point. A 2× cursor is half its pixel size
    /// on screen.
    pub scale: i32,
    /// The hotspot from `wl_pointer.set_cursor`, in surface-local LOGICAL coordinates.
    pub hotspot: (i32, i32),
}

/// What the host should show as the pointer cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostCursor {
    /// `wl_pointer.set_cursor` with a null surface: the client draws no cursor at all.
    Hidden,
    /// A themed shape, to be satisfied from the host's own cursor set.
    Shape(CursorShape),
    /// Client-supplied pixels, to be drawn verbatim at the given hotspot.
    Image(CursorImage),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_round_trips_through_its_css_name() {
        for shape in CursorShape::all() {
            assert_eq!(CursorShape::from_name(shape.css_name()), Some(shape));
        }
        assert_eq!(CursorShape::all().count(), 34);
    }

    #[test]
    fn an_unknown_cursor_name_is_not_silently_mapped() {
        assert_eq!(CursorShape::from_name("nonesuch"), None);
        assert_eq!(CursorShape::from_name(""), None);
    }
}
