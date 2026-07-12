//! `wl_output` / `xdg_output` handler. Smithay's `OutputManagerState` owns the mode/scale/geometry
//! tables and emits the `geometry`/`mode`/`scale`/`done` events; the compositor only needs to declare
//! that it participates. Output state changes are pushed via `Output::change_current_state` (see
//! `DdState::new`), not through this handler.

use smithay::wayland::output::OutputHandler;

use crate::DdState;

impl OutputHandler for DdState {}
