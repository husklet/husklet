use super::*;

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
                let (surface, buffer) = (app.surface.clone().unwrap(), app.buffer.clone().unwrap());
                surface.attach(Some(&buffer), 0, 0);
                surface.damage(0, 0, i32::MAX, i32::MAX);
                let _cb: WlCallback = surface.frame(qh, ());
                surface.commit();
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
impl Dispatch<WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        e: <WlPointer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // SOURCE side: on the button press that anchors the implicit grab, start the drag.
        if let wl_pointer::Event::Button {
            serial,
            button,
            state,
            ..
        } = e
        {
            if button == BTN_LEFT && matches!(state, WEnum::Value(ButtonState::Pressed)) {
                if let (Some(dd), Some(source), Some(surface)) =
                    (&app.dd, &app.source, &app.surface)
                {
                    dd.start_drag(Some(source), surface, None, serial);
                }
            }
        }
    }
}
impl Dispatch<WlDataSource, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlDataSource,
        e: <WlDataSource as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_source::Event::Send { mime_type, fd } = e {
            assert_eq!(mime_type, MIME, "source asked for the mime it offered");
            let mut f = std::fs::File::from(fd);
            f.write_all(PAYLOAD).expect("write drag payload");
            app.source_send_fired = true;
        }
    }
}
impl Dispatch<WlDataDevice, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlDataDevice,
        e: <WlDataDevice as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_data_device::Event::Enter {
                serial, x, y, id, ..
            } => {
                let offer = id.expect("drag enter carries a data offer");
                // Negotiate the drop: accept the mime + a Copy action, so the release actually delivers `drop`.
                offer.accept(serial, Some(MIME.to_string()));
                offer.set_actions(DndAction::Copy, DndAction::Copy);
                app.current_offer = Some(offer);
                app.events
                    .push(DndEv::Enter(x.round() as i32, y.round() as i32));
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                app.events
                    .push(DndEv::Motion(x.round() as i32, y.round() as i32));
            }
            wl_data_device::Event::Leave => {
                app.events.push(DndEv::Leave);
                app.offered_mimes.clear();
            }
            wl_data_device::Event::Drop => {
                app.events.push(DndEv::Drop);
            }
            _ => {}
        }
    }
    wayland_client::event_created_child!(App, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}
impl Dispatch<WlDataOffer, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlDataOffer,
        e: <WlDataOffer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_data_offer::Event::Offer { mime_type } => app.offered_mimes.push(mime_type),
            wl_data_offer::Event::SourceActions { source_actions } => {
                if let WEnum::Value(actions) = source_actions {
                    app.source_actions = Some(actions);
                }
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
    XdgToplevel,
    WlDataDeviceManager
);
