//! `zwp_text_input_v3` — text-input for editors, address bars, forms, and IME composition.
//!
//! ## Why a hand-written dispatch instead of Smithay's `TextInputManagerState`
//! Smithay ships a `text_input` module, but it is hard-wired to the companion `zwp_input_method_v2`
//! protocol: every client text-input request is *discarded* unless a separate input-method CLIENT is
//! bound to the seat (`if !input_method_handle.has_instance() { return; }`), and the only way to emit
//! `commit_string`/`preedit_string` back to the app is for that input-method client to relay it. That
//! model fits a Linux desktop where ibus/fcitx run as their own Wayland clients.
//!
//! dd has no such client: the HOST (macOS, via its own input-method framework / marked-text) is the
//! IME, and `dd-compositor` is the bridge. So the compositor itself must be the input-method endpoint —
//! it owns the text-input instances directly and synthesizes `commit_string`/`preedit_string` from host
//! input. This module therefore implements the `zwp_text_input_manager_v3` + `zwp_text_input_v3`
//! dispatch on [`DdState`] directly (no input-method client required), which is what lets a guest text
//! field actually receive committed text.
//!
//! ## What it does
//!   - Advertises `zwp_text_input_manager_v3` so toolkits (GTK/Qt/Chromium) that bind text-input find it
//!     and enable their IME path instead of misbehaving on its absence.
//!   - Follows keyboard focus: `enter`/`leave` are emitted from [`DdState::set_text_input_focus`], which
//!     [`DdState::focus_surface`] drives — text-input focus tracks the keyboard focus, per the protocol.
//!   - Tracks the double-buffered client state (enable/disable, surrounding text, content-type, cursor
//!     rectangle) applied atomically on `commit`, enforcing the one-active-text-input-per-seat rule and
//!     the "ignore requests from an unfocused instance" rule.
//!   - Exposes [`DdState::text_input_commit_string`] / [`DdState::text_input_preedit_string`] /
//!     [`DdState::text_input_delete_surrounding`] so the host input path can route committed / preedit
//!     text (and surrounding-text edits) into the focused, enabled text field with the correct `done`
//!     serial. Plain keystroke typing still flows through `wl_keyboard`; this is the IME-composition seam
//!     on top of it (CJK, dead keys, emoji, the macOS marked-text path).
//!
//! Every request handler is defensive: a request naming an unknown/destroyed instance, or arriving from a
//! client that does not hold text-input focus, is ignored rather than trusted — a client cannot steer
//! another client's text field or crash the compositor with an out-of-order request.

use smithay::reexports::wayland_server::{
    backend::{ClientId, GlobalId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::IsAlive;

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ZwpTextInputV3},
};

use crate::DdState;

/// The `zwp_text_input_manager_v3` version we advertise. Only content-hint flags gate on v2; the
/// enter/leave/commit_string/preedit_string/done event surface this bridge relies on is all v1.
const TEXT_INPUT_VERSION: u32 = 1;

/// Per-`zwp_text_input_v3` user data. The instance is identified by its protocol object id (matched in
/// [`TextInputState`]); no per-object payload is needed, so this is a marker.
#[derive(Debug, Default)]
pub struct TextInputData;

/// The compositor-owned aggregate text-input state: the manager global, every bound text-input instance
/// (across all clients), and the surface that currently holds text-input focus (kept equal to keyboard
/// focus). Lives in [`DdState`].
pub struct TextInputState {
    /// The `zwp_text_input_manager_v3` global id (advertised in the registry).
    #[allow(dead_code)]
    global: GlobalId,
    /// Every live `zwp_text_input_v3` instance. Small (typically one per focusable client), so a linear
    /// scan is cheaper than a map and keeps ordering deterministic.
    instances: Vec<TextInputInstance>,
    /// The surface with text-input focus — mirrors keyboard focus (see [`DdState::set_text_input_focus`]).
    focus: Option<WlSurface>,
}

/// One bound text-input object plus its committed/pending state.
struct TextInputInstance {
    obj: ZwpTextInputV3,
    /// Count of client `commit` requests — echoed as the `done` serial so the client can correlate our
    /// `commit_string`/`preedit_string` with the state revision it committed (the v3 serial handshake).
    serial: u32,
    /// Whether this instance is enabled (a committed `enable`) for the focused surface. Only an active
    /// instance receives `commit_string`/`preedit_string`.
    active: bool,
    /// Double-buffered state, accumulated by the state requests and applied on `commit`.
    pending: PendingState,
    /// Applied content purpose (`zwp_text_input_v3.content_purpose`), if the client set one. Lets the
    /// host tailor its IME panel (e.g. a numeric keyboard for `digits`); surfaced via
    /// [`DdState::text_input_content_purpose`].
    content_purpose: Option<u32>,
    /// Applied content hint bitmask, if set.
    content_hint: Option<u32>,
    /// Applied surrounding text `(text, cursor, anchor)`, if the client reported it.
    surrounding: Option<(String, i32, i32)>,
    /// Applied cursor rectangle `(x, y, w, h)` in surface-local coords, if set — where the host would
    /// anchor an IME candidate window.
    cursor_rect: Option<(i32, i32, i32, i32)>,
}

/// The double-buffered pending state a text-input accumulates between `commit` requests.
#[derive(Default)]
struct PendingState {
    enable: Option<bool>,
    surrounding: Option<(String, i32, i32)>,
    content_type: Option<(u32, u32)>,
    cursor_rect: Option<(i32, i32, i32, i32)>,
    change_cause: Option<u32>,
}

impl TextInputInstance {
    /// Reset every applied + pending field. Called on `enable` (the protocol invalidates prior state) and
    /// whenever text-input focus enters/leaves this instance.
    fn reset(&mut self) {
        self.active = false;
        self.pending = PendingState::default();
        self.content_purpose = None;
        self.content_hint = None;
        self.surrounding = None;
        self.cursor_rect = None;
    }
}

impl TextInputState {
    /// Create the `zwp_text_input_manager_v3` global and an empty instance table.
    pub fn new(dh: &DisplayHandle) -> TextInputState {
        let global =
            dh.create_global::<DdState, ZwpTextInputManagerV3, ()>(TEXT_INPUT_VERSION, ());
        TextInputState {
            global,
            instances: Vec::new(),
            focus: None,
        }
    }

    fn add_instance(&mut self, obj: &ZwpTextInputV3) {
        self.instances.push(TextInputInstance {
            obj: obj.clone(),
            serial: 0,
            active: false,
            pending: PendingState::default(),
            content_purpose: None,
            content_hint: None,
            surrounding: None,
            cursor_rect: None,
        });
    }

    fn remove_instance(&mut self, obj: &ZwpTextInputV3) {
        let id = obj.id();
        self.instances.retain(|i| i.obj.id() != id);
    }

    fn index_of(&self, obj: &ZwpTextInputV3) -> Option<usize> {
        let id = obj.id();
        self.instances.iter().position(|i| i.obj.id() == id)
    }

    /// Apply a committed state revision for `resource`. Enforces the protocol's one-active-per-seat rule
    /// (an `enable` while another instance is active is ignored) and the "state before enable is ignored"
    /// rule. Mutates only the addressed instance.
    fn apply_commit(&mut self, resource: &ZwpTextInputV3) {
        let Some(idx) = self.index_of(resource) else {
            return;
        };
        let pending = std::mem::take(&mut self.instances[idx].pending);
        match pending.enable {
            Some(true) => {
                // Ignore an enable while a different instance is already the active text input.
                let another_active = self
                    .instances
                    .iter()
                    .enumerate()
                    .any(|(j, i)| j != idx && i.active);
                if another_active {
                    return;
                }
                // `enable` invalidates all prior state, then re-applies whatever was committed with it.
                self.instances[idx].reset();
                self.instances[idx].active = true;
            }
            Some(false) => {
                self.instances[idx].active = false;
                return;
            }
            None => {
                // A commit with no prior committed enable carries no applicable state.
                if !self.instances[idx].active {
                    return;
                }
            }
        }
        let inst = &mut self.instances[idx];
        if let Some(s) = pending.surrounding {
            inst.surrounding = Some(s);
        }
        if let Some((hint, purpose)) = pending.content_type {
            inst.content_hint = Some(hint);
            inst.content_purpose = Some(purpose);
        }
        if let Some(r) = pending.cursor_rect {
            inst.cursor_rect = Some(r);
        }
        let _ = pending.change_cause; // accepted; no host-side effect yet.
    }
}

impl DdState {
    /// Move text-input focus to `surface` (or clear it), mirroring keyboard focus. Sends `leave` to the
    /// previously focused client's text-input instances and `enter` to the newly focused client's — per
    /// the protocol, text-input focus follows the keyboard when the seat has the keyboard capability.
    /// Both transitions invalidate the affected instances' state (the client must resend it after `enter`).
    /// A no-op when the focus target is unchanged.
    pub(crate) fn set_text_input_focus(&mut self, surface: Option<WlSurface>) {
        let unchanged = match (&self.text_input.focus, &surface) {
            (Some(a), Some(b)) => a.id() == b.id(),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }

        // leave: notify the old focus's instances, then invalidate all instance state.
        if let Some(old) = self.text_input.focus.take() {
            if old.alive() {
                let old_id = old.id();
                for inst in &self.text_input.instances {
                    if inst.obj.id().same_client_as(&old_id) {
                        inst.obj.leave(&old);
                    }
                }
            }
        }
        for inst in self.text_input.instances.iter_mut() {
            inst.reset();
        }

        // enter: notify the new focus's instances.
        self.text_input.focus = surface.clone();
        if let Some(new) = surface {
            if new.alive() {
                let new_id = new.id();
                for inst in &self.text_input.instances {
                    if inst.obj.id().same_client_as(&new_id) {
                        inst.obj.enter(&new);
                    }
                }
            }
        }
    }

    /// Whether the focused client currently has an ENABLED text input. The host input path consults this
    /// to decide whether composed/marked text should be routed as `commit_string` (a text field is
    /// listening) rather than only as raw `wl_keyboard` keystrokes.
    pub fn text_input_active(&self) -> bool {
        self.active_focused_index().is_some()
    }

    /// The content purpose of the focused, active text input (`0` = normal … `13` = terminal), if one is
    /// enabled and the client set a purpose. Lets the host pick an appropriate on-screen keyboard / IME
    /// mode.
    pub fn text_input_content_purpose(&self) -> Option<u32> {
        self.active_focused_index()
            .and_then(|i| self.text_input.instances[i].content_purpose)
    }

    /// Route committed text into the focused, enabled text field: emit `commit_string(text)` followed by
    /// `done(serial)`. This is the host→guest IME seam (macOS marked-text confirm, an emoji pick, a CJK
    /// candidate selection). Returns whether a field received it (false = no focused, enabled text input).
    pub fn text_input_commit_string(&mut self, text: &str) -> bool {
        let Some(idx) = self.active_focused_index() else {
            return false;
        };
        let inst = &self.text_input.instances[idx];
        inst.obj.commit_string(Some(text.to_string()));
        inst.obj.done(inst.serial);
        true
    }

    /// Set the composing (pre-edit) string on the focused, enabled text field: emit `preedit_string` +
    /// `done`. `cursor_begin`/`cursor_end` are byte offsets into `text` (both `-1` hides the cursor).
    /// Returns whether a field received it.
    pub fn text_input_preedit_string(
        &mut self,
        text: &str,
        cursor_begin: i32,
        cursor_end: i32,
    ) -> bool {
        let Some(idx) = self.active_focused_index() else {
            return false;
        };
        let inst = &self.text_input.instances[idx];
        inst.obj
            .preedit_string(Some(text.to_string()), cursor_begin, cursor_end);
        inst.obj.done(inst.serial);
        true
    }

    /// Ask the focused, enabled text field to delete `before` bytes before and `after` bytes after the
    /// cursor (an IME replacing surrounding text): emit `delete_surrounding_text` + `done`. Returns whether
    /// a field received it.
    pub fn text_input_delete_surrounding(&mut self, before: u32, after: u32) -> bool {
        let Some(idx) = self.active_focused_index() else {
            return false;
        };
        let inst = &self.text_input.instances[idx];
        inst.obj.delete_surrounding_text(before, after);
        inst.obj.done(inst.serial);
        true
    }

    /// Index into `text_input.instances` of the active text input belonging to the focused client, if any.
    fn active_focused_index(&self) -> Option<usize> {
        let focus = self.text_input.focus.as_ref().filter(|s| s.alive())?;
        let fid = focus.id();
        self.text_input
            .instances
            .iter()
            .position(|i| i.active && i.obj.id().same_client_as(&fid))
    }
}

// ---- zwp_text_input_manager_v3: the factory global ---------------------------------------------------

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for DdState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for DdState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // get_text_input(id, seat): mint a text-input for the (single) seat. If the requesting client
            // already holds text-input focus, send `enter` immediately so a field created after focus does
            // not miss it (matches Smithay's "enter on late-bound instance" behaviour).
            zwp_text_input_manager_v3::Request::GetTextInput { id, seat: _ } => {
                let obj = data_init.init(id, TextInputData);
                state.text_input.add_instance(&obj);
                if let Some(focus) = state.text_input.focus.clone() {
                    if focus.alive() && obj.id().same_client_as(&focus.id()) {
                        obj.enter(&focus);
                    }
                }
            }
            zwp_text_input_manager_v3::Request::Destroy => {}
            _ => {}
        }
    }
}

// ---- zwp_text_input_v3: one text entry field --------------------------------------------------------

impl Dispatch<ZwpTextInputV3, TextInputData> for DdState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &TextInputData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // The `done` serial counts EVERY client commit, so bump it before any focus/validity gate — a
        // client that keeps committing while unfocused must not desync the serial it expects back.
        if matches!(request, zwp_text_input_v3::Request::Commit) {
            if let Some(idx) = state.text_input.index_of(resource) {
                state.text_input.instances[idx].serial =
                    state.text_input.instances[idx].serial.wrapping_add(1);
            }
        }

        // Per protocol, after a `leave` the compositor must ignore requests from any text-input instance
        // until the next `enter`. Enforce it by dropping requests from a client that is not the current
        // text-input focus — this also stops a background client steering the focused field.
        let focused_by_this_client = state
            .text_input
            .focus
            .as_ref()
            .filter(|s| s.alive())
            .map(|s| resource.id().same_client_as(&s.id()))
            .unwrap_or(false);
        if !focused_by_this_client {
            return;
        }

        match request {
            zwp_text_input_v3::Request::Enable => {
                if let Some(idx) = state.text_input.index_of(resource) {
                    state.text_input.instances[idx].pending.enable = Some(true);
                }
            }
            zwp_text_input_v3::Request::Disable => {
                if let Some(idx) = state.text_input.index_of(resource) {
                    state.text_input.instances[idx].pending.enable = Some(false);
                }
            }
            zwp_text_input_v3::Request::SetSurroundingText {
                text,
                cursor,
                anchor,
            } => {
                if let Some(idx) = state.text_input.index_of(resource) {
                    state.text_input.instances[idx].pending.surrounding =
                        Some((text, cursor, anchor));
                }
            }
            zwp_text_input_v3::Request::SetTextChangeCause { cause } => {
                if let Some(idx) = state.text_input.index_of(resource) {
                    state.text_input.instances[idx].pending.change_cause = Some(u32::from(cause));
                }
            }
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                if let Some(idx) = state.text_input.index_of(resource) {
                    state.text_input.instances[idx].pending.content_type =
                        Some((u32::from(hint), u32::from(purpose)));
                }
            }
            zwp_text_input_v3::Request::SetCursorRectangle {
                x,
                y,
                width,
                height,
            } => {
                if let Some(idx) = state.text_input.index_of(resource) {
                    state.text_input.instances[idx].pending.cursor_rect =
                        Some((x, y, width, height));
                }
            }
            zwp_text_input_v3::Request::Commit => {
                state.text_input.apply_commit(resource);
            }
            zwp_text_input_v3::Request::Destroy => {}
            _ => {}
        }
    }

    /// The text-input object was destroyed (explicitly, or because the client disconnected). Drop it from
    /// the instance table so a later focus change / commit never touches a dead resource.
    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &ZwpTextInputV3,
        _data: &TextInputData,
    ) {
        state.text_input.remove_instance(resource);
    }
}
