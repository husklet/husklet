use super::*;
impl CompositorHandler for HlState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        self.register_surface(surface);
    }

    /// A `wl_subsurface` was created (`wl_subcompositor.get_subsurface(surface, parent)`). Establish the
    /// scene parent linkage immediately: subsurfaces map SYNC by default (they present atomically with the
    /// parent until `set_desync`), at offset `(0, 0)` until the first `set_position` commit. The concrete
    /// offset / sync mode / z-order are refreshed from Smithay on each commit (`sync_subsurface_tree`).
    fn new_subsurface(&mut self, surface: &WlSurface, parent: &WlSurface) {
        if let (Some(sid), Some(parent_sid)) = (self.sid(surface), self.sid(parent)) {
            self.engine.scene.set_role(
                sid,
                SurfaceRole::Subsurface(SubsurfaceState {
                    parent: parent_sid,
                    x: 0,
                    y: 0,
                    sync: true,
                }),
            );
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.on_commit(surface);
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        self.teardown_surface(surface);
    }
}

impl BufferHandler for HlState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {
        #[cfg(feature = "macos-surface")]
        if let Ok(dmabuf) = get_dmabuf(_buffer) {
            let external = dmabuf.weak();
            if let Some(token) = self.native_buffers.remove(&external) {
                self.destroy_native_buffer(token, &external);
            }
        }
    }
}

impl ShmHandler for HlState {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl DmabufHandler for HlState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    /// A client finished a `zwp_linux_buffer_params_v1` create/create_immed. Accept the buffer iff it is
    /// something this SOFTWARE presenter can truthfully turn into pixels — a single-plane LINEAR
    /// ARGB/XRGB (or byte-swapped ABGR/XBGR) buffer, whose plane fd is plain CPU memory we `pread` at
    /// commit ([`read_dmabuf_rgba`]). REJECT anything else (a tiled/GPU modifier we cannot detile without
    /// a GPU, or a multi-plane/YUV layout we do not unpack) via `notifier.failed()`, so the client falls
    /// back to `wl_shm` rather than committing a buffer we could only ever present as blank. Honest by
    /// construction: we never accept an import we cannot actually read.
    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let fmt = dmabuf.format();
        let linear = dmabuf.num_planes() == 1
            && matches!(
                fmt.code,
                Fourcc::Argb8888 | Fourcc::Xrgb8888 | Fourcc::Abgr8888 | Fourcc::Xbgr8888
            )
            && fmt.modifier == Modifier::Linear;
        #[cfg(feature = "macos-surface")]
        let external_token = (self.native_frames_enabled()
            && dmabuf.num_planes() == 1
            && matches!(fmt.code, Fourcc::Argb8888 | Fourcc::Xrgb8888)
            && fmt.modifier == Modifier::from(hl_surface_protocol::buffer::MODIFIER))
        .then(|| super::buffer::ExternalBuffer::token(&dmabuf))
        .flatten();
        #[cfg(feature = "macos-surface")]
        let mut external_registration = None;
        #[cfg(feature = "macos-surface")]
        let external = external_token.is_some_and(|token| {
            let external = dmabuf.weak();
            // One Wayland resource owns one registration. Replacing an existing weak key would lose the
            // earlier resource's unregister and desynchronize the token refcount.
            if self.native_buffers.contains_key(&external) {
                return false;
            }
            if !self.register_native_token(token) {
                return false;
            }
            if self
                .native_buffers
                .insert(external.clone(), token)
                .is_some()
            {
                self.unregister_native_token(token);
                return false;
            }
            external_registration = Some((external, token));
            true
        });
        #[cfg(not(feature = "macos-surface"))]
        let external = false;
        if linear || external {
            if notifier.successful::<HlState>().is_err() {
                #[cfg(feature = "macos-surface")]
                if let Some((external, token)) = external_registration {
                    self.native_buffers.remove(&external);
                    self.unregister_native_token(token);
                }
            }
        } else {
            notifier.failed();
        }
    }
}

impl SeatHandler for HlState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<HlState> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<HlState>, _focused: Option<&WlSurface>) {}

    /// The focused client set the pointer cursor. A `wp_cursor_shape_device_v1.set_shape` arrives here as
    /// [`CursorImageStatus::Named`] (Smithay decoded the shape enum to a `CursorIcon`); a client-provided
    /// `wl_pointer.set_cursor` with a surface arrives as `Surface`, and hiding it as `Hidden`. Each is
    /// forwarded to the host through [`Windows::set_cursor`] — the compositor advertises both mechanisms,
    /// so both must reach the platform. The requested NAMED shape is additionally recorded into
    /// [`Observations`]: `wp_cursor_shape` carries no reply event, so that is how a test asserts Chrome's
    /// cursor name reached the seat. `Surface`/`Hidden` clear the recorded name.
    fn cursor_image(
        &mut self,
        _seat: &Seat<HlState>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        use crate::scene::port::{CursorShape, HostCursor};
        use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
        let named = match image {
            CursorImageStatus::Named(icon) => {
                let name = icon.name().to_string();
                // An unknown name is not silently mapped onto some other shape: the host falls back to its
                // default cursor, which is uninformative rather than wrong.
                let shape = CursorShape::from_name(&name).unwrap_or(CursorShape::Default);
                self.cursor_surface = None;
                self.cursor_pixels = None;
                self.engine
                    .presenter_mut()
                    .set_cursor(&HostCursor::Shape(shape));
                Some(name)
            }
            CursorImageStatus::Hidden => {
                self.cursor_surface = None;
                self.cursor_pixels = None;
                self.engine.presenter_mut().set_cursor(&HostCursor::Hidden);
                None
            }
            CursorImageStatus::Surface(cursor) => {
                // A client-provided cursor image becomes a host cursor, never a window. Mark it with the
                // scene's `Cursor` role so the commit path stops treating it as its own window root:
                // roleless, it composes a stray frame per cursor update and — having no window to arm a
                // repaint against — strands the surface's `wl_surface.frame` callbacks, stalling a client
                // that animates its cursor.
                if let Some(sid) = self.sid(&cursor) {
                    self.engine.scene.set_role(sid, SurfaceRole::Cursor);
                    let hotspot = with_states(&cursor, |states| {
                        states
                            .data_map
                            .get::<CursorImageSurfaceData>()
                            .map(|attributes| {
                                let hotspot = attributes.lock().unwrap().hotspot;
                                (hotspot.x, hotspot.y)
                            })
                            .unwrap_or((0, 0))
                    });
                    self.cursor_surface = Some((sid, hotspot));
                    // `set_cursor` may follow the cursor surface's commit, in which case the pixels are
                    // already stashed and the host cursor can be built right now.
                    self.publish_host_cursor();
                }
                None
            }
        };
        if let Some(name) = &named {
            hl_debug!(
                tag::WAYLAND,
                "wp_cursor_shape set_shape -> named cursor '{name}'"
            );
        }
        self.observations.lock().unwrap().cursor_shape = named;
    }
}

/// Selection (clipboard / primary) plumbing for `wl_data_device`. Headless we carry no per-selection
/// user data (`()`), and the default `new_selection` / `send_selection` (no-op / not-answered) are
/// sufficient: the compositor never provides a selection of its OWN (that is what `send_selection`
/// would answer). CLIENT-to-client transfer is fully live — when the offered source is another client's
/// `wl_data_source`, Smithay forwards the receiving client's `wl_data_offer.receive` fd straight to the
/// source client as `wl_data_source.send`, and the source writes the bytes over that pipe. Combined with
/// clipboard focus tracking (via [`set_data_device_focus`], driven from keyboard focus in
/// [`Self::set_keyboard_focus`]), this makes a real cross-client copy/paste round-trip work headless —
/// see the `clipboard_selection_roundtrip` demo, which asserts the exact bytes A "copies" are what B
/// "pastes".
impl SelectionHandler for HlState {
    type SelectionUserData = String;

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if ty != SelectionTarget::Clipboard {
            return;
        }
        let Some(source) = source else { return };
        let mime_types = source.mime_types();
        hl_debug!(
            tag::WAYLAND,
            "clipboard client source mimes={:?}",
            mime_types
        );
        let mime = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"]
            .into_iter()
            .find(|candidate| mime_types.iter().any(|mime| mime == candidate));
        let Some(mime) = mime else { return };
        let Ok((mut reader, writer)) = UnixStream::pair() else {
            return;
        };
        source.send(mime.to_owned(), writer.into());
        let tx = self.clipboard_tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if reader.read_to_end(&mut bytes).is_ok() {
                hl_debug!(tag::WAYLAND, "clipboard client bytes={}", bytes.len());
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let _ = tx.send(text);
            }
        });
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        _mime_type: String,
        fd: std::os::fd::OwnedFd,
        _seat: Seat<Self>,
        text: &Self::SelectionUserData,
    ) {
        if ty != SelectionTarget::Clipboard {
            return;
        }
        let text = text.clone();
        std::thread::spawn(move || {
            let mut file = std::fs::File::from(fd);
            let _ = file.write_all(text.as_bytes());
        });
    }
}

/// A client-initiated drag-and-drop grab (a client dragging one of its surfaces). The neutral headless
/// compositor applies no DnD policy of its own — the client manages the data transfer — but the two grab
/// lifecycle callbacks are recorded into the shared [`Observations`] so a test can observe the SOURCE side
/// of the drag (which emits no client-visible wire event beyond the grab itself): `started` fires when a
/// `start_drag` is honoured (Smithay has replaced the implicit pointer grab with its DnD grab, so the drag
/// pointer path is now live and a test may inject the motion that carries the offer to a target), and
/// `dropped` fires when the user releases the last button, carrying whether the drop was NEGOTIATED
/// (`validated` — the target accepted a mime + a non-empty action, so `wl_data_device.drop` was delivered).
/// See the `drag_and_drop` demo.
impl ClientDndGrabHandler for HlState {
    fn started(
        &mut self,
        _source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>,
        _icon: Option<WlSurface>,
        _seat: Seat<HlState>,
    ) {
        hl_debug!(tag::WAYLAND, "dnd started");
        let mut o = self.observations.lock().unwrap();
        o.dnd_active = true;
        o.dnd_dropped = false;
        o.dnd_drop_validated = false;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, validated: bool, _seat: Seat<HlState>) {
        hl_debug!(tag::WAYLAND, "dnd dropped validated={}", validated);
        let mut o = self.observations.lock().unwrap();
        o.dnd_active = false;
        o.dnd_dropped = true;
        o.dnd_drop_validated = validated;
    }
}

/// A server-initiated drag-and-drop grab (the compositor starting a DnD). Never initiated headless, so
/// the default (empty) handler is correct.
impl ServerDndGrabHandler for HlState {}
