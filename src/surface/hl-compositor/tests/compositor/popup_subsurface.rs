use super::*;

impl Dispatch<WlRegistry, GlobalListContents> for AppData {
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

impl Dispatch<XdgWmBase, ()> for AppData {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, Role> for AppData {
    fn event(
        app: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as Proxy>::Event,
        role: &Role,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            match role {
                Role::Toplevel if !app.tl_drawn => {
                    app.tl_surface.attach(Some(&app.tl_buffer), 0, 0);
                    app.tl_surface.damage(0, 0, TL_W, TL_H);
                    let _cb: WlCallback = app.tl_surface.frame(qh, ());
                    app.tl_surface.commit();
                    app.tl_drawn = true;
                }
                Role::Popup if !app.pop_drawn => {
                    // The popup's initial configure carries its resolved geometry; now paint it.
                    app.pop_surface.attach(Some(&app.pop_buffer), 0, 0);
                    app.pop_surface.damage(0, 0, POP_W, POP_H);
                    app.pop_surface.commit();
                    app.pop_drawn = true;
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgPopup, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &XdgPopup,
        event: <XdgPopup as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            app.pop_done = true;
        }
    }
}

impl Dispatch<WlBuffer, ()> for AppData {
    fn event(
        app: &mut Self,
        buffer: &WlBuffer,
        event: <WlBuffer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_buffer::Event::Release = event {
            if buffer.id() == app.tl_buffer.id() {
                app.tl_released = true;
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        event: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            app.tl_frame_done = true;
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
                _: <$t as Proxy>::Event,
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
    WlSeat,
    WlSubcompositor,
    WlSubsurface,
    XdgToplevel,
    XdgPositioner
);
