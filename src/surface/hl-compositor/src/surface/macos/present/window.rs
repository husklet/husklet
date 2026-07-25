use super::*;

impl MacPresenter {
    pub(super) fn reconcile_native_window(&mut self, desired: &WindowState) {
        let state = self
            .surfaces
            .entry(desired.surface)
            .or_insert_with(SurfState::new);
        let previous = state.desired.clone();
        if previous.as_ref().map(|window| window.visibility) != Some(desired.visibility) {
            hl_debug!(
                tag::PRESENT,
                "native window surface={} visibility={:?}->{:?}",
                desired.surface.0,
                previous.as_ref().map(|window| window.visibility),
                desired.visibility
            );
        }
        let parent = match desired.kind {
            WindowKind::Toplevel { parent } => parent,
            WindowKind::Popup { parent, .. } => Some(parent),
        };
        state.desired = Some(desired.clone());
        if let Some(window) = &state.window {
            if previous
                .as_ref()
                .is_none_or(|previous| previous.title != desired.title)
            {
                window.set_title(&desired.title);
            }
            if previous.as_ref().is_none_or(|previous| {
                previous.min_size != desired.min_size || previous.max_size != desired.max_size
            }) {
                window.set_size_constraints(desired.min_size, desired.max_size);
            }
            if previous.as_ref().is_none_or(|previous| {
                previous.maximized != desired.maximized || previous.fullscreen != desired.fullscreen
            }) {
                window.set_mode(desired.maximized, desired.fullscreen);
                state.reported_native_size = Some(window.logical_size());
                state.native_resize_pending = None;
                state.native_resize_last_sent = None;
            }
            if previous
                .as_ref()
                .is_none_or(|previous| previous.visibility != desired.visibility)
            {
                window.set_visibility(desired.visibility);
            }
        }
        let previous_parent = previous.as_ref().and_then(|window| match window.kind {
            WindowKind::Toplevel { parent } => parent,
            WindowKind::Popup { parent, .. } => Some(parent),
        });
        if previous_parent != parent {
            if let Some(window) = self
                .surfaces
                .get(&desired.surface)
                .and_then(|state| state.window.as_ref())
            {
                window.detach_parent();
            }
        }
        if let Some(parent) = parent.filter(|_| previous_parent != parent) {
            if let (Some(owner), Some(child)) = (
                self.surfaces
                    .get(&parent)
                    .and_then(|state| state.window.as_ref()),
                self.surfaces
                    .get(&desired.surface)
                    .and_then(|state| state.window.as_ref()),
            ) {
                owner.add_child(child);
            }
        }
    }
}
