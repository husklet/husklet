use super::*;

/// Double-buffered `wp_tearing_control_v1` presentation hint committed with surface state.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TearingControlCachedState {
    pub(super) hint: u32,
}

impl Cacheable for TearingControlCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }
    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Surface-local flag: does this `wl_surface` already have a live `wp_tearing_control_v1`? Enforces the
/// protocol's one-object-per-surface rule (a second `get_tearing_control` is a `tearing_control_exists`
/// protocol error, not a silent overwrite). Set on create, cleared on the object's `destroy`.
#[derive(Debug, Default)]
struct TearingControlSurfaceData {
    attached: std::sync::atomic::AtomicBool,
}

/// User data of a `wp_tearing_control_v1` object — a weak handle to the `wl_surface` it controls, so
/// `set_presentation_hint` / `destroy` can find the surface whose cached hint to update.
#[derive(Debug)]
struct TearingControlUserData(Mutex<Weak<WlSurface>>);

impl TearingControlUserData {
    fn wl_surface(&self) -> Option<WlSurface> {
        self.0.lock().unwrap().upgrade().ok()
    }
}

impl GlobalDispatch<WpTearingControlManagerV1, ()> for HlState {
    fn bind(
        _state: &mut HlState,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpTearingControlManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, HlState>,
    ) {
        hl_debug!(tag::WAYLAND, "wp_tearing_control_manager_v1 bound");
        data_init.init(resource, ());
    }
}

impl Dispatch<WpTearingControlManagerV1, ()> for HlState {
    fn request(
        _state: &mut HlState,
        _client: &Client,
        manager: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, HlState>,
    ) {
        use std::sync::atomic::Ordering;
        match request {
            wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } => {
                // Enforce one `wp_tearing_control_v1` per surface (protocol error otherwise).
                let already = with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(TearingControlSurfaceData::default);
                    let data = states.data_map.get::<TearingControlSurfaceData>().unwrap();
                    if data.attached.load(Ordering::Acquire) {
                        true
                    } else {
                        data.attached.store(true, Ordering::Release);
                        false
                    }
                });
                if already {
                    manager.post_error(
                        wp_tearing_control_manager_v1::Error::TearingControlExists,
                        "wl_surface already has a wp_tearing_control_v1",
                    );
                } else {
                    data_init.init(id, TearingControlUserData(Mutex::new(surface.downgrade())));
                }
            }
            wp_tearing_control_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WpTearingControlV1, TearingControlUserData> for HlState {
    fn request(
        _state: &mut HlState,
        _client: &Client,
        _resource: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        data: &TearingControlUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, HlState>,
    ) {
        use std::sync::atomic::Ordering;
        match request {
            wp_tearing_control_v1::Request::SetPresentationHint { hint } => {
                let Some(surface) = data.wl_surface() else {
                    return;
                };
                // `async` = tearing allowed (wire 1); `vsync` (or any unknown value) = do not tear (wire 0).
                let value = match hint {
                    WEnum::Value(PresentationHint::Async) => 1,
                    _ => 0,
                };
                hl_debug!(
                    tag::WAYLAND,
                    "wp_tearing_control set_presentation_hint -> {}",
                    if value == 1 { "async" } else { "vsync" }
                );
                with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<TearingControlCachedState>()
                        .pending()
                        .hint = value;
                });
            }
            wp_tearing_control_v1::Request::Destroy => {
                // Destroying the object resets the surface to `vsync` at its next commit and frees the
                // per-surface slot so a fresh `wp_tearing_control_v1` may be created.
                if let Some(surface) = data.wl_surface() {
                    with_states(&surface, |states| {
                        states
                            .cached_state
                            .get::<TearingControlCachedState>()
                            .pending()
                            .hint = 0;
                        if let Some(sd) = states.data_map.get::<TearingControlSurfaceData>() {
                            sd.attached.store(false, Ordering::Release);
                        }
                    });
                }
            }
            _ => {}
        }
    }
}
