use super::*;

#[derive(Debug)]
struct IdentityData {
    surface: Weak<WlSurface>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssociationError {
    InvalidSerial,
    Pending,
}

fn validate_association(
    serial: u64,
    last: Option<u64>,
    pending: bool,
) -> Result<(), AssociationError> {
    if serial == 0 || last.is_some_and(|last| serial <= last) {
        return Err(AssociationError::InvalidSerial);
    }
    if pending {
        return Err(AssociationError::Pending);
    }
    Ok(())
}

impl HlState {
    fn mint_surface_token(&mut self, surface: SurfaceId) -> Result<u64, getrandom::Error> {
        if let Some(token) = self.surface_tokens.get(&surface) {
            return Ok(*token);
        }
        loop {
            let mut bytes = [0_u8; 8];
            getrandom::fill(&mut bytes)?;
            let token = u64::from_ne_bytes(bytes);
            if token != 0 && !self.token_surfaces.contains_key(&token) {
                self.surface_tokens.insert(surface, token);
                self.token_surfaces.insert(token, surface);
                #[cfg(feature = "macos-surface")]
                let _ = self.register_native_token(token);
                // The scene learns the surface now HAS content, and the window is reconciled against
                // that immediately. A zero-copy client's first frame arrives through the GPU service,
                // not as a `wl_buffer`, so without this the toplevel stays occluded and no native
                // window is ever created — and the frame it is waiting for can never be shown.
                self.engine.scene.set_native_token(surface, Some(token));
                self.reconcile_window(surface);
                return Ok(token);
            }
        }
    }

    pub(super) fn take_surface_association(&mut self, surface: SurfaceId) -> Option<(u64, u64)> {
        let token = *self.surface_tokens.get(&surface)?;
        let serial = self.pending_associations.remove(&surface)?;
        Some((token, serial))
    }

    #[cfg(feature = "macos-surface")]
    pub(super) fn take_commit_association(
        &mut self,
        surface: SurfaceId,
        external: Option<(u64, u64)>,
    ) -> Option<(u64, u64)> {
        match external {
            Some(association) => Some(association),
            None => self.take_surface_association(surface),
        }
    }

    pub(super) fn retire_surface_identity(&mut self, surface: SurfaceId) {
        if let Some(token) = self.surface_tokens.remove(&surface) {
            #[cfg(feature = "macos-surface")]
            self.retire_native_token(token);
            self.token_surfaces.remove(&token);
            // Paired with the mint: the scene must not keep claiming content through an identity that
            // no longer exists.
            self.engine.scene.set_native_token(surface, None);
        }
        self.pending_associations.remove(&surface);
        self.last_associations.remove(&surface);
        self.identity_surfaces.retain(|_, value| *value != surface);
    }
}

impl GlobalDispatch<HlSurfaceManagerV1, ()> for HlState {
    fn bind(
        _state: &mut HlState,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<HlSurfaceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, HlState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<HlSurfaceManagerV1, ()> for HlState {
    fn request(
        state: &mut HlState,
        _client: &Client,
        _manager: &HlSurfaceManagerV1,
        request: hl_surface_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, HlState>,
    ) {
        if let hl_surface_manager_v1::Request::GetSurface { id, surface } = request {
            let Some(sid) = state.sid(&surface) else {
                return;
            };
            let identity = data_init.init(
                id,
                IdentityData {
                    surface: surface.downgrade(),
                },
            );
            if state
                .identity_surfaces
                .values()
                .any(|registered| *registered == sid)
            {
                identity.post_error(
                    hl_surface_identity_v1::Error::Retired,
                    "surface already has an identity",
                );
                return;
            }
            let token = match state.mint_surface_token(sid) {
                Ok(token) => token,
                Err(error) => {
                    identity.post_error(
                        hl_surface_identity_v1::Error::Retired,
                        format!("surface identity entropy unavailable: {error}"),
                    );
                    return;
                }
            };
            state.identity_surfaces.insert(identity.id(), sid);
            identity.token((token >> 32) as u32, token as u32);
        }
    }
}

impl Dispatch<HlSurfaceIdentityV1, IdentityData> for HlState {
    fn request(
        state: &mut HlState,
        _client: &Client,
        identity: &HlSurfaceIdentityV1,
        request: hl_surface_identity_v1::Request,
        data: &IdentityData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, HlState>,
    ) {
        match request {
            hl_surface_identity_v1::Request::Associate {
                serial_high,
                serial_low,
            } => {
                let serial = (u64::from(serial_high) << 32) | u64::from(serial_low);
                let Some(surface) = data.surface.upgrade().ok() else {
                    identity.post_error(hl_surface_identity_v1::Error::Retired, "surface retired");
                    return;
                };
                let Some(sid) = state.sid(&surface) else {
                    identity.post_error(hl_surface_identity_v1::Error::Retired, "surface retired");
                    return;
                };
                if state.identity_surfaces.get(&identity.id()) != Some(&sid)
                    || !state.surface_tokens.contains_key(&sid)
                {
                    identity.post_error(hl_surface_identity_v1::Error::Retired, "identity retired");
                    return;
                }
                match validate_association(
                    serial,
                    state.last_associations.get(&sid).copied(),
                    state.pending_associations.contains_key(&sid),
                ) {
                    Ok(()) => {}
                    Err(AssociationError::InvalidSerial) => {
                        identity.post_error(
                            hl_surface_identity_v1::Error::InvalidSerial,
                            "serial must be nonzero and strictly increasing",
                        );
                        return;
                    }
                    Err(AssociationError::Pending) => {
                        identity.post_error(
                            hl_surface_identity_v1::Error::Pending,
                            "surface already has an association awaiting commit",
                        );
                        return;
                    }
                }
                state.last_associations.insert(sid, serial);
                state.pending_associations.insert(sid, serial);
            }
            hl_surface_identity_v1::Request::Destroy => {
                if let Some(sid) = state.identity_surfaces.remove(&identity.id()) {
                    state.retire_surface_identity(sid);
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut HlState,
        _client: ClientId,
        identity: &HlSurfaceIdentityV1,
        _data: &IdentityData,
    ) {
        if let Some(sid) = state.identity_surfaces.remove(&identity.id()) {
            state.retire_surface_identity(sid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_must_be_nonzero_and_increasing() {
        assert_eq!(
            validate_association(0, None, false),
            Err(AssociationError::InvalidSerial)
        );
        assert_eq!(
            validate_association(4, Some(4), false),
            Err(AssociationError::InvalidSerial)
        );
        assert_eq!(
            validate_association(3, Some(4), false),
            Err(AssociationError::InvalidSerial)
        );
        assert_eq!(validate_association(5, Some(4), false), Ok(()));
    }

    #[test]
    fn only_one_association_may_await_commit() {
        assert_eq!(
            validate_association(5, Some(4), true),
            Err(AssociationError::Pending)
        );
    }

    #[cfg(feature = "macos-surface")]
    #[test]
    fn external_association_leaves_protocol_association_for_the_next_commit() {
        use smithay::reexports::wayland_server::Display;

        let display: Display<HlState> = Display::new().unwrap();
        let mut state = HlState::new(
            &display.handle(),
            crate::adapter::smithay::present::PngPresenter::new(),
        );
        let surface = state.engine.scene.create_surface();
        state.surface_tokens.insert(surface, 41);
        state.pending_associations.insert(surface, 7);

        assert_eq!(
            state.take_commit_association(surface, Some((99, 3))),
            Some((99, 3))
        );
        assert_eq!(state.pending_associations.get(&surface), Some(&7));
        assert_eq!(state.take_commit_association(surface, None), Some((41, 7)));
        assert!(!state.pending_associations.contains_key(&surface));
    }
}
