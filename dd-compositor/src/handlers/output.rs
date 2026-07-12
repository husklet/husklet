//! `wl_output` / `xdg_output` handler. Smithay's [`OutputManagerState`] owns the mode/scale/geometry
//! tables and emits the `geometry`/`mode`/`scale`/`done` events; the compositor only needs to declare
//! that it participates. Output state changes are pushed via `Output::change_current_state` (see
//! `DdState::new`), not through this handler.
//!
//! ## Multi-output readiness
//! The primary output (`dd-0`) is stood up in `DdState::new`. The state is NOT hard-wired to exactly
//! one output, though: [`OutputManagerState`] (created with `new_with_xdg_output`) tracks *every*
//! [`Output`] whose global is created, and independently emits `wl_output` + `zxdg_output_v1`
//! (name/description/logical position+size) for each. [`DdState::add_output`] registers an additional
//! output global at a logical position, and the extras are retained in `DdState::extra_outputs` so a
//! multi-monitor guest sees a coherent output layout even though the present path still shows one
//! native window per surface. Toolkits (GTK/Qt) that require xdg-output for correct multi-monitor +
//! scaling get it here.

use smithay::output::{Mode as OutMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::{Point, Size};
use smithay::wayland::output::OutputHandler;

use crate::{DdState, OUTPUT_REFRESH_MHZ};

impl OutputHandler for DdState {}

impl DdState {
    /// Register an ADDITIONAL output global (beyond the primary `dd-0`) at a logical `position`, with its
    /// own name/description, device `mode` (px), and integer `scale`. The new output's `wl_output` +
    /// `zxdg_output_v1` are advertised immediately by the shared [`OutputManagerState`]; the returned (and
    /// retained) [`Output`] lets the compositor later re-state it. Proves the output plumbing handles
    /// `>1` output cleanly for multi-monitor guests.
    pub fn add_output(
        &mut self,
        name: &str,
        model: &str,
        mode_px: (i32, i32),
        scale: i32,
        position: (i32, i32),
    ) -> Output {
        let output = Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "dd".into(),
                model: model.into(),
            },
        );
        output.create_global::<Self>(&self.dh);
        let mode = OutMode {
            size: Size::from((mode_px.0.max(1), mode_px.1.max(1))),
            refresh: OUTPUT_REFRESH_MHZ as i32,
        };
        output.change_current_state(
            Some(mode),
            None,
            Some(Scale::Integer(scale.max(1))),
            Some(Point::from((position.0, position.1))),
        );
        output.set_preferred(mode);
        self.extra_outputs.push(output.clone());
        output
    }
}
