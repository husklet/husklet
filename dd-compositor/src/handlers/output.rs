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
        let recovering_from_headless = self.headless;
        let output = Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "dd".into(),
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
        let fallback = std::iter::once(&self.output).chain(self.extra_outputs.iter())
            .find(|output| output.name() != name).cloned();
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
