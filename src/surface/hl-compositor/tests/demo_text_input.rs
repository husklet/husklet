//! DEMO — `text_input` (a real `zwp_text_input_v3` IME round-trip: preedit + commit reach the client).
//!
//! The text-entry path GTK/Chrome/Qt need for an on-screen keyboard / IME. A client binds
//! `zwp_text_input_manager_v3`, gets a text-input on its focused surface, and `enable`s it; the compositor
//! then delivers `preedit_string` (transient composing text) and `commit_string` (accepted text) —
//! double-buffered, applied on each `done` — and the client's committed text updates EXACTLY.
//!
//! Smithay routes `zwp_text_input_v3` through an input method (`zwp_input_method_v2`): a text-input request
//! is only honoured, `enter` only sent, and events only delivered while an input method instance exists on
//! the seat. So this test's single client ALSO acts as a minimal in-process IME "backend" — it binds
//! `zwp_input_method_manager_v2` and `get_input_method(seat)`, making `has_instance()` true — which is the
//! real, honest shape of Wayland text input (an app + an IME daemon on the same compositor). The host IME
//! seam ([`InputCommand::ImePreeditString`] / [`InputCommand::ImeCommitString`] /
//! [`InputCommand::ImeDeleteSurrounding`]) delivers the events; Smithay stamps the matching `done` serial.
//!
//! Asserts an EXACT IME string round-trip:
//!   * `enter` is delivered once the client is focused (IME present);
//!   * a preedit "hel" shows as composing text (committed still empty);
//!   * a commit "hello" replaces the preedit → committed text == "hello";
//!   * a delete_surrounding(2,0) then commit "p!" → committed text == "help!".

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_keyboard::WlKeyboard, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ZwpTextInputV3},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2, zwp_input_method_v2::ZwpInputMethodV2,
};

const W: i32 = 180;
const H: i32 = 120;
const COLOR: [u8; 4] = [0x30, 0x30, 0x38, 0xFF];

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    // ---- text-input client state ----
    entered: bool,
    /// The number of `commit` REQUESTS this client has issued on its text-input — the value the
    /// compositor's `done` serial must equal for the client to apply the pending state.
    commit_count: u32,
    /// The committed (real) text.
    committed: String,
    /// The current preedit (composing) overlay — NOT part of the committed text.
    preedit: String,
    preedit_cursor: (i32, i32),
    // ---- pending (double-buffered) IME state, applied on `done` ----
    pending_commit: Option<String>,
    pending_preedit: Option<(String, i32, i32)>,
    pending_delete: Option<(u32, u32)>,
}

impl App {
    /// Apply the pending double-buffered IME state on a `done` whose serial matches our commit count
    /// (per zwp_text_input_v3: replace preedit, delete surrounding, insert commit, set new preedit).
    fn apply_done(&mut self, serial: u32) {
        if serial != self.commit_count {
            // Stale — the compositor computed this against a different state; ignore it (spec-mandated).
            self.pending_commit = None;
            self.pending_preedit = None;
            self.pending_delete = None;
            return;
        }
        // 1. Existing preedit is transient; it is replaced this cycle.
        self.preedit.clear();
        // 2. Delete requested surrounding text (cursor is at the end of the committed text here).
        if let Some((before, _after)) = self.pending_delete.take() {
            let new_len = self.committed.len().saturating_sub(before as usize);
            self.committed.truncate(new_len);
        }
        // 3. Insert the commit string.
        if let Some(text) = self.pending_commit.take() {
            self.committed.push_str(&text);
        }
        // 5. Set the new preedit overlay.
        if let Some((text, cb, ce)) = self.pending_preedit.take() {
            self.preedit = text;
            self.preedit_cursor = (cb, ce);
        }
    }
}

#[test]
fn text_input() {
    let h = Harness::start("text_input");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    let ti_manager: ZwpTextInputManagerV3 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_text_input_manager_v3");
    let im_manager: ZwpInputMethodManagerV2 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_input_method_manager_v2");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "ti", W, H, &solid(W, H, COLOR));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-text-input".into());
    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer,
        drawn: false,
        frame_done: false,
        entered: false,
        commit_count: 0,
        committed: String::new(),
        preedit: String::new(),
        preedit_cursor: (0, 0),
        pending_commit: None,
        pending_preedit: None,
        pending_delete: None,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // Become the IME backend FIRST (so `has_instance()` is true before focus), then the text-input client.
    let _input_method: ZwpInputMethodV2 = im_manager.get_input_method(&seat, &qh, ());
    let _text_input: ZwpTextInputV3 = ti_manager.get_text_input(&seat, &qh, ());
    let _ = queue.roundtrip(&mut app);

    // Focus the toplevel → with an IME present, the compositor sends `zwp_text_input_v3.enter`. On enter the
    // client enables text input and commits (its only commit → commit_count == 1, matching the compositor).
    h.input_tx
        .send(InputCommand::FocusTopmostKeyboard)
        .expect("focus");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.entered {
        assert!(
            Instant::now() < deadline,
            "text-input never received `enter`"
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.commit_count, 1,
        "client enabled + committed exactly once on enter"
    );
    // Ensure the compositor has PROCESSED the enable+commit (so the text-input is ACTIVE — the one-shot IME
    // injects below are dropped if it is not) before driving the IME. A roundtrip syncs the server past them.
    let _ = queue.roundtrip(&mut app);

    // ---- preedit "hel": composing text shows, committed still empty ----
    h.input_tx
        .send(InputCommand::ImePreeditString {
            text: "hel".into(),
            cursor_begin: 0,
            cursor_end: 3,
        })
        .expect("preedit");
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.preedit == "hel"),
        "preedit never became \"hel\" (got {:?})",
        app.preedit,
    );
    assert_eq!(
        app.committed, "",
        "preedit is transient — committed text is still empty"
    );
    assert_eq!(
        app.preedit_cursor,
        (0, 3),
        "preedit cursor byte range delivered exactly"
    );

    // ---- commit "hello": replaces the preedit, committed text becomes "hello" ----
    h.input_tx
        .send(InputCommand::ImeCommitString("hello".into()))
        .expect("commit hello");
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.committed == "hello"),
        "committed text never became \"hello\" (got {:?})",
        app.committed,
    );
    assert_eq!(app.preedit, "", "commit cleared the preedit overlay");

    // ---- delete_surrounding(2,0) then commit "p!": "hello" -> "hel" -> "help!" ----
    h.input_tx
        .send(InputCommand::ImeDeleteSurrounding {
            before_length: 2,
            after_length: 0,
        })
        .expect("delete surrounding");
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.committed == "hel"),
        "delete_surrounding(2,0) never trimmed \"hello\" to \"hel\" (got {:?})",
        app.committed,
    );
    h.input_tx
        .send(InputCommand::ImeCommitString("p!".into()))
        .expect("commit p!");
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.committed == "help!"),
        "committed text never became \"help!\" (got {:?})",
        app.committed,
    );

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(_kbd);
    h.shutdown();
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        e: <XdgWmBase as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.drawn {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        e: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.frame_done = true;
        }
    }
}
impl Dispatch<ZwpTextInputV3, ()> for App {
    fn event(
        app: &mut Self,
        ti: &ZwpTextInputV3,
        e: <ZwpTextInputV3 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            zwp_text_input_v3::Event::Enter { .. } => {
                // Enable text input and commit our request state (the client's single commit).
                ti.enable();
                ti.commit();
                app.commit_count += 1;
                app.entered = true;
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                app.entered = false;
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                app.pending_preedit = Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                app.pending_commit = Some(text.unwrap_or_default());
            }
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                app.pending_delete = Some((before_length, after_length));
            }
            zwp_text_input_v3::Event::Done { serial } => {
                app.apply_done(serial);
            }
            _ => {}
        }
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSeat,
    WlKeyboard,
    XdgToplevel,
    ZwpTextInputManagerV3,
    ZwpInputMethodManagerV2,
    ZwpInputMethodV2
);
