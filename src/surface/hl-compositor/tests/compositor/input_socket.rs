use super::*;

impl Dispatch<WlRegistry, GlobalListContents> for AppData {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for AppData {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for AppData {
    fn event(
        app: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if !app.configured {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.configured = true;
            }
        }
    }
}

impl Dispatch<WlBuffer, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlBuffer,
        event: <WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_buffer::Event::Release = event {
            app.released = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        event: <WlCallback as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            app.frame_done = true;
        }
    }
}

impl Dispatch<WlSeat, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_seat::Event::Capabilities { capabilities } = event {
            if let WEnum::Value(caps) = capabilities {
                app.seat_caps = Some(caps);
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        event: <WlPointer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                app.pointer_enter = Some((surface.id() == app.surface.id(), surface_x, surface_y));
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                app.pointer_motion = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let pressed = matches!(state, WEnum::Value(ButtonState::Pressed));
                app.pointer_button = Some((button, pressed));
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                if matches!(axis, WEnum::Value(wl_pointer::Axis::VerticalScroll)) {
                    app.pointer_axis_vert = Some(value);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => {
                app.keyboard_enter = Some(surface.id() == app.surface.id());
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = matches!(state, WEnum::Value(KeyState::Pressed));
                app.keyboard_key = Some((key, pressed));
            }
            _ => {}
        }
    }
}

// Objects whose events we don't act on.
macro_rules! ignore_dispatch {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for AppData {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {}
        }
    )*};
}
ignore_dispatch!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    XdgToplevel,
    WlOutput
);
