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
            // First configure: attach the colored buffer, damage the whole surface, request a frame
            // callback, and commit.
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

impl Dispatch<WlOutput, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlOutput,
        event: <WlOutput as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Geometry { .. } => app.output_geometry = true,
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                // Record the CURRENT mode's pixel size (we only advertise one, marked current+preferred).
                if matches!(flags, WEnum::Value(m) if m.contains(wl_output::Mode::Current)) {
                    app.output_mode_px = Some((width, height));
                }
            }
            wl_output::Event::Scale { factor } => app.output_scale = Some(factor),
            wl_output::Event::Name { name } => app.output_name = Some(name),
            wl_output::Event::Done => app.output_done = true,
            _ => {}
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
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let WEnum::Value(caps) = capabilities {
                app.seat_caps = Some(caps);
            }
        }
    }
}

// The remaining objects emit events we don't need to act on (the keyboard's `keymap` fd, pointer motion,
// etc.) — creating them and observing they stay alive is the fidelity the seat assertions prove.
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
    WlPointer,
    WlKeyboard,
    WlDataDeviceManager
);

// `wl_data_device` can emit `data_offer` (which would create a child `wl_data_offer`), `selection`, and
// DnD enter/leave/motion/drop. None occur in this headless, single-client test (no selection is set and
// no drag is started), so we only need to observe the device stays alive after the roundtrip.
impl Dispatch<WlDataDevice, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        event: <WlDataDevice as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // No cross-client selection/DnD is exercised here, so no event is expected. Any that arrives is a
        // surprise worth failing on (e.g. a spurious selection offer to a single client).
        panic!("unexpected wl_data_device event in headless single-client test: {event:?}");
    }
}
