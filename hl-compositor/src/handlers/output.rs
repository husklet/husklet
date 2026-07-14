//! `wl_output` / `xdg_output` handler. Smithay's [`OutputManagerState`] owns the mode/scale/geometry
//! tables and emits the `geometry`/`mode`/`scale`/`done` events; the compositor only needs to declare
//! that it participates. Output state changes are pushed via `Output::change_current_state` (see
//! `HlState::new`), not through this handler.
//!
//! ## Multi-output readiness
//! The primary output (`hl-0`) is stood up in `HlState::new`. The state is NOT hard-wired to exactly
//! one output, though: [`OutputManagerState`] (created with `new_with_xdg_output`) tracks *every*
//! [`Output`] whose global is created, and independently emits `wl_output` + `zxdg_output_v1`
//! (name/description/logical position+size) for each. [`HlState::add_output`] registers an additional
//! output global at a logical position, and the extras are retained in `HlState::extra_outputs` so a
//! multi-monitor guest sees a coherent output layout even though the present path still shows one
//! native window per surface. Toolkits (GTK/Qt) that require xdg-output for correct multi-monitor +
//! scaling get it here.

use smithay::output::{Mode as OutMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::{Point, Size};
use smithay::wayland::output::OutputHandler;

use crate::{HlState, OUTPUT_REFRESH_MHZ};

impl OutputHandler for HlState {}

impl HlState {
    pub(crate) fn selected_output(&self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> Output {
        self.surface_outputs
            .get(&self.surface_id(surface))
            .cloned()
            .unwrap_or_else(|| self.output.clone())
    }

    /// Transactionally move a surface tree to an advertised output: enter the replacement before
    /// leaving the old output, update preferred scale, then make subsequent presentation use it.
    pub fn route_surface_to_output(&mut self, sid: u32, output_name: &str) -> bool {
        let Some(surface) = self.surface_resources.get(&sid).cloned() else { return false; };
        let Some(next) = std::iter::once(&self.output)
            .chain(self.extra_outputs.iter())
            .find(|output| output.name() == output_name)
            .cloned() else { return false; };
        let root = self.window_root(&surface).unwrap_or(surface);
        let mut tree = Vec::new();
        self.collect_tree_surfaces(&root, &mut tree);
        for (popup, _, _) in self.collect_popups_for_root(&root) {
            self.collect_tree_surfaces(&popup, &mut tree);
        }
        // Target validation and tree discovery complete before the first mutation. Each member enters
        // the replacement before its membership flips and before leaving its previous output.
        for member in tree {
            let member_sid = self.surface_id(&member);
            let old = self.selected_output(&member);
            if old == next { continue; }
            next.enter(&member);
            self.surface_outputs.insert(member_sid, next.clone());
            self.send_preferred_fractional_scale(&member);
            old.leave(&member);
            self.dirty.insert(member_sid);
        }
        true
    }

    /// The advertised (primary + extra) outputs, in a stable order (primary first).
    fn outputs(&self) -> Vec<Output> {
        std::iter::once(&self.output).chain(self.extra_outputs.iter()).cloned().collect()
    }

    /// An output's logical rectangle `(x, y, w, h)`: its `current_location` (logical position set at
    /// registration) plus its device mode divided by the integer scale. `None` when it has no mode.
    fn output_logical_rect(output: &Output) -> Option<(i32, i32, i32, i32)> {
        let mode = output.current_mode()?;
        let scale = output.current_scale().integer_scale().max(1);
        let loc = output.current_location();
        Some((loc.x, loc.y, (mode.size.w / scale).max(1), (mode.size.h / scale).max(1)))
    }

    /// The advertised output whose logical rectangle a surface at logical rect `(x, y, w, h)` overlaps
    /// most (largest intersection area). Falls back to [`Self::nearest_output`] when the surface overlaps
    /// no output (fully off every screen), so routing always resolves to a concrete output. This is the
    /// geometry-driven analogue of the explicit `route_surface_to_output` membership: a surface is a
    /// member of the output it actually covers, which drives its `wl_surface.enter`, preferred scale, and
    /// presentation target.
    pub fn output_for_geometry(&self, x: i32, y: i32, w: i32, h: i32) -> Option<Output> {
        let mut best: Option<(i64, Output)> = None;
        for output in self.outputs() {
            let Some((ox, oy, ow, oh)) = Self::output_logical_rect(&output) else { continue; };
            let ix = (x + w).min(ox + ow) - x.max(ox);
            let iy = (y + h).min(oy + oh) - y.max(oy);
            let area = if ix > 0 && iy > 0 { ix as i64 * iy as i64 } else { 0 };
            if area > 0 && best.as_ref().is_none_or(|(a, _)| area > *a) {
                best = Some((area, output));
            }
        }
        best.map(|(_, output)| output).or_else(|| self.nearest_output(x, y, w, h))
    }

    /// The advertised output whose logical rectangle center is closest to the surface rect center — the
    /// deterministic placement target when a surface overlaps no output (e.g. it is dragged entirely into
    /// a gap, or its only output was hot-unplugged). Primary output wins ties (iteration order).
    pub fn nearest_output(&self, x: i32, y: i32, w: i32, h: i32) -> Option<Output> {
        let (cx, cy) = (x + w / 2, y + h / 2);
        let mut best: Option<(i64, Output)> = None;
        for output in self.outputs() {
            let Some((ox, oy, ow, oh)) = Self::output_logical_rect(&output) else { continue; };
            let (ocx, ocy) = (ox + ow / 2, oy + oh / 2);
            let d = (cx - ocx) as i64 * (cx - ocx) as i64 + (cy - ocy) as i64 * (cy - ocy) as i64;
            if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, output));
            }
        }
        best.map(|(_, output)| output)
    }

    /// Route a surface tree to the output its on-screen logical rectangle `(x, y, w, h)` actually
    /// intersects most (geometry-driven membership), migrating enter/leave + preferred scale if it moved.
    /// Returns whether a migration occurred. This is what a live window-move/resize feeds so a window
    /// dragged across the seam between two outputs updates its `wl_surface.enter`/`leave` and rescales.
    pub fn route_surface_by_geometry(&mut self, sid: u32, x: i32, y: i32, w: i32, h: i32) -> bool {
        let Some(target) = self.output_for_geometry(x, y, w, h) else { return false; };
        if self.surface_outputs.get(&sid) == Some(&target) {
            return false;
        }
        self.route_surface_to_output(sid, &target.name())
    }

    /// A deterministic fallback output EXCLUDING `exclude`, primary preferred — the placement target when
    /// `exclude` is hot-unplugged.
    fn fallback_output(&self, exclude: &str) -> Option<Output> {
        self.outputs().into_iter().find(|output| output.name() != exclude)
    }

    /// Host display-configuration notification: a new physical output was connected. Registers it as an
    /// advertised output (its `wl_output`/`xdg_output` appear immediately) — the host-driven entry point
    /// the platform loop calls when the macOS display arrangement changes.
    pub fn on_host_output_connected(
        &mut self,
        name: &str,
        model: &str,
        mode_px: (i32, i32),
        scale: i32,
        position: (i32, i32),
    ) -> Output {
        self.add_output(name, model, mode_px, scale, position)
    }

    /// Host display-configuration notification: a physical output was disconnected. Atomically retires its
    /// `wl_output` global, migrates every surface that lived on it to a fallback output (entering the
    /// replacement BEFORE leaving the removed output and re-sending its preferred scale), and re-issues a
    /// fullscreen configure at the new output's size for any migrated fullscreen toplevel — so the client
    /// repaints at the correct size AFTER it has entered the new output. Returns whether the output existed.
    pub fn on_host_output_disconnected(&mut self, name: &str) -> bool {
        self.output_disconnected(name)
    }

    /// The `wl_output` hot-unplug path (see [`Self::on_host_output_disconnected`]). Named for the host
    /// notification it services.
    pub fn output_disconnected(&mut self, name: &str) -> bool {
        // Capture the surfaces on the doomed output before it is removed, so fullscreen toplevels among
        // them can be reconfigured at their NEW output's size once migrated.
        let removed = self.outputs().into_iter().find(|output| output.name() == name);
        let affected: Vec<u32> = match removed.as_ref() {
            Some(removed) => self
                .surface_outputs
                .iter()
                .filter_map(|(sid, output)| (output == removed).then_some(*sid))
                .collect(),
            None => Vec::new(),
        };
        if !self.remove_output(name) {
            return false;
        }
        // Migration (enter new → leave old) already happened inside `remove_output`; now, AFTER the
        // migrated surfaces have entered their replacement output, re-issue the fullscreen configure so a
        // fullscreen client repaints at the new output's logical size in the correct event order.
        self.reconfigure_migrated_fullscreen(&affected);
        true
    }

    pub(crate) fn inherit_output_membership(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        root: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        let target = self.selected_output(root);
        let sid = self.surface_id(surface);
        let old = self.selected_output(surface);
        if old == target { return; }
        target.enter(surface);
        self.surface_outputs.insert(sid, target);
        self.send_preferred_fractional_scale(surface);
        old.leave(surface);
    }

    pub fn surface_output_name(&self, sid: u32) -> Option<String> {
        self.surface_outputs.get(&sid).map(Output::name)
    }

    /// Register an ADDITIONAL output global (beyond the primary `hl-0`) at a logical `position`, with its
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
        let recovering_from_headless = self.headless;
        let output = Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "hl".into(),
                model: model.into(),
            },
        );
        let global = output.create_global::<Self>(&self.dh);
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
        self.output_globals.insert(output.name(), global);
        if self.headless {
            self.output = output.clone();
        } else {
            self.extra_outputs.push(output.clone());
        }
        if recovering_from_headless {
            let mut live: Vec<(u32, _)> = self.surface_resources
                .iter().map(|(sid, surface)| (*sid, surface.clone())).collect();
            live.sort_by_key(|(sid, _)| *sid);
            for (sid, surface) in live {
                output.enter(&surface);
                self.surface_outputs.insert(sid, output.clone());
                self.send_preferred_fractional_scale(&surface);
                self.dirty.insert(sid);
            }
            self.headless = false;
        }
        output
    }

    pub fn remove_output(&mut self, name: &str) -> bool {
        let Some(global) = self.output_globals.get(name).cloned() else { return false; };
        let removed = std::iter::once(&self.output).chain(self.extra_outputs.iter())
            .find(|output| output.name() == name).cloned().expect("output record missing");
        let fallback = self.fallback_output(name);
        let affected: Vec<u32> = self.surface_outputs.iter()
            .filter_map(|(sid, output)| (output == &removed).then_some(*sid)).collect();
        if let Some(next) = fallback.as_ref() {
            for sid in affected { self.route_surface_to_output(sid, &next.name()); }
        } else {
            for sid in affected {
                if let Some(surface) = self.surface_resources.get(&sid) { removed.leave(surface); }
                self.surface_outputs.remove(&sid);
                self.dirty.insert(sid);
            }
            self.headless = true;
        }
        self.dh.remove_global::<Self>(global);
        self.output_globals.remove(name);
        self.extra_outputs.retain(|output| output.name() != name);
        if self.output.name() == name {
            if let Some(next) = fallback {
                self.output = next.clone();
                self.extra_outputs.retain(|output| output != &next);
            }
        }
        true
    }

    pub fn is_headless(&self) -> bool { self.headless }
}
