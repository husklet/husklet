use super::*;

impl HlState {
    pub(in crate::adapter::smithay::state) fn update_output_membership(&mut self, sid: SurfaceId) {
        let Some(root) = self.engine.scene.window_root(sid) else {
            return;
        };
        if !matches!(
            self.engine.scene.get(root).map(|s| &s.role),
            Some(SurfaceRole::Toplevel)
        ) {
            return;
        }
        let Some(wl_surface) = self.surfaces_by_id.get(&root).cloned() else {
            return;
        };
        let mapped = self.engine.scene.get(root).and_then(|s| s.buffer).is_some();
        let current = self.entered_outputs.get(&root).copied();
        let target = self.engine.scene.selected_output(root).map(|o| o.id);
        if mapped {
            let Some(target) = target else { return };
            if current != Some(target) {
                // Leave the output we were on (if any) before entering the new one, so a client observes a
                // clean handoff (leave A, then enter B) rather than being on two outputs at once.
                if let Some(cur) = current {
                    if let Some(handle) = self.wl_output_handle(cur) {
                        handle.leave(&wl_surface);
                    }
                }
                if let Some(handle) = self.wl_output_handle(target) {
                    handle.enter(&wl_surface);
                }
                self.entered_outputs.insert(root, target);
            }
        } else if let Some(cur) = current {
            if let Some(handle) = self.wl_output_handle(cur) {
                handle.leave(&wl_surface);
            }
            self.entered_outputs.remove(&root);
        }
    }

    /// The smithay `wl_output` handle for a neutral [`OutputId`], if advertised.
    pub(in crate::adapter::smithay::state) fn wl_output_handle(
        &self,
        id: OutputId,
    ) -> Option<&WlOutputHandle> {
        self.outputs
            .iter()
            .find(|(oid, _)| *oid == id)
            .map(|(_, h)| h)
    }

    /// The primary output's `wl_output` handle (the first advertised) — the fallback the presentation
    /// feedback names when a surface has no resolvable selected output.
    pub(in crate::adapter::smithay::state) fn primary_wl_output(&self) -> Option<&WlOutputHandle> {
        self.outputs.first().map(|(_, h)| h)
    }

    /// The integer scale of the output surface `sid`'s window root is displayed on (its selected output,
    /// else the primary). Sources the fractional-scale hint so a surface on a HiDPI output learns a larger
    /// preferred scale than one on a scale-1 output.
    pub(in crate::adapter::smithay::state) fn output_scale_for(&self, sid: SurfaceId) -> i32 {
        let root = self.engine.scene.window_root(sid).unwrap_or(sid);
        self.engine
            .scene
            .selected_output(root)
            .map(|o| o.scale.max(1))
            .unwrap_or(1)
    }

    /// (Re)send `wp_fractional_scale_v1.preferred_scale` for `sid` from its current output's scale. A no-op
    /// if the client created no `wp_fractional_scale_v1` on the surface, or if the value is unchanged
    /// (smithay's `set_preferred_scale` dedups) — so it is safe to call on every route change.
    pub(in crate::adapter::smithay::state) fn send_preferred_fractional_scale(
        &self,
        sid: SurfaceId,
    ) {
        let scale = self.output_scale_for(sid) as f64;
        if let Some(surface) = self.surfaces_by_id.get(&sid) {
            with_states(surface, |states| {
                with_fractional_scale(states, |fractional| {
                    fractional.set_preferred_scale(scale);
                });
            });
        }
    }

    /// Route the toplevel at index `n` (ascending surface-id order) to the output whose logical rectangle
    /// contains global logical point `(x, y)`, then emit the resulting `wl_surface.leave`/`enter` and
    /// refresh its preferred fractional scale. The host/window-manager seam a multi-output demo drives to
    /// "place" a window on a monitor: real position-based routing (the compositor decides which output a
    /// window is on from where it sits), reduced to the smallest correct form — a point tested against each
    /// output's `logical_rect`. A point outside every output, or an out-of-range index, is ignored.
    pub(in crate::adapter::smithay::state) fn move_toplevel_to_point(
        &mut self,
        n: usize,
        x: i32,
        y: i32,
    ) {
        let Some(root) = self.toplevel_at(n) else {
            return;
        };
        let Some(output_id) = self.output_at_point(x, y) else {
            return;
        };
        self.engine.scene.route_surface_to_output(root, output_id);
        self.update_output_membership(root);
        self.send_preferred_fractional_scale(root);
    }

    /// The neutral [`OutputId`] whose logical rectangle contains global logical point `(x, y)`, if any.
    pub(in crate::adapter::smithay::state) fn output_at_point(
        &self,
        x: i32,
        y: i32,
    ) -> Option<OutputId> {
        self.engine
            .scene
            .outputs()
            .iter()
            .find(|o| o.contains_point(x, y))
            .map(|o| o.id)
    }
}
