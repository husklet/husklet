//! `zwp_tablet_manager_v2` — graphics-tablet / stylus support (Krita, GIMP, Inkscape probe this at
//! startup even on machines with no tablet). Composed from the vendored Smithay `tablet_manager`
//! module; the [`smithay::wayland::tablet_manager::TabletSeatHandler`] impl already lives in
//! `handlers::seat` (it was required for `wp_cursor_shape`'s tablet-tool cursor routing).
//!
//! ## Host policy (no tablet hardware)
//! dd has no stylus/digitizer, so the seat advertises ZERO tablets and tools: a client binds the
//! manager, calls `get_tablet_seat(seat)`, obtains a live `zwp_tablet_seat_v2`, and receives no
//! `tablet_added` — the exact protocol picture of a seat without a tablet, which is what every
//! toolkit's no-tablet path already handles. Hot-plugging a tablet later is a single
//! `seat.tablet_seat().add_tablet::<HlState>(dh, &desc)` call (the mechanism the roundtrip test drives
//! through a virtual descriptor to prove `tablet_added` reaches the client through this delegate).

use smithay::{
    reexports::wayland_server::DisplayHandle,
    wayland::tablet_manager::{TabletDescriptor, TabletManagerState, TabletSeatTrait},
};

use crate::HlState;

impl HlState {
    /// Construct the `zwp_tablet_manager_v2` global. Kept in this module so the tablet slice owns its
    /// registration; the returned state must be stored in [`HlState`] to keep the global alive.
    pub(crate) fn new_tablet_manager(dh: &DisplayHandle) -> TabletManagerState {
        TabletManagerState::new::<Self>(dh)
    }

    /// Advertise a tablet on the primary seat (a real digitizer hot-plug would call this). dd adds none
    /// by default — this exists so the composed delegate can be proven to deliver `tablet_added` to a
    /// bound `zwp_tablet_seat_v2`, and so a future host tablet bridge has a ready seam.
    pub fn add_tablet(&mut self, name: &str) {
        let dh = self.dh.clone();
        self.seat.tablet_seat().add_tablet::<Self>(
            &dh,
            &TabletDescriptor {
                name: name.to_string(),
                usb_id: None,
                syspath: None,
            },
        );
    }
}
