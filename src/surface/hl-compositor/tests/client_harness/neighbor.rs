use super::*;

pub struct Neighbor {
    pub conn: Connection,
    pub queue: EventQueue<NeighborApp>,
    pub app: NeighborApp,
    pub width: i32,
    pub height: i32,
    pub color: [u8; 4],
}

/// Dispatch state for a [`Neighbor`]. A distinct type from any demo's own client `App`, so both coexist
/// in one test binary.
pub struct NeighborApp {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

impl Neighbor {
    /// Connect a fresh well-behaved client on the shared socket, map a `w`x`h` solid-`color` toplevel,
    /// and block until it is mapped and has drawn its first frame. Panics (fails the test) if the
    /// compositor never completes the handshake — the exact symptom of an adapter that a prior hostile
    /// client wedged.
    pub fn map(dir: &Path, tag: &str, w: i32, h: i32, color: [u8; 4]) -> Neighbor {
        let conn = Connection::connect_to_env().expect("neighbor connect_to_env");
        let (globals, mut queue) =
            registry_queue_init::<NeighborApp>(&conn).expect("neighbor registry init");
        let qh = queue.handle();

        let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

        let buffer = make_buffer(&shm, &qh, dir, tag, w, h, &solid(w, h, color));
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title(format!("neighbor-{tag}"));
        surface.commit();

        let mut app = NeighborApp {
            surface: surface.clone(),
            buffer,
            drawn: false,
            frame_done: false,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(app.drawn && app.frame_done) {
            assert!(
                Instant::now() < deadline,
                "neighbor {tag} never mapped (adapter wedged?)"
            );
            queue
                .blocking_dispatch(&mut app)
                .expect("neighbor dispatch map");
        }
        // Keep the shell objects alive for the client's lifetime.
        std::mem::forget(toplevel);
        std::mem::forget(xdg);
        Neighbor {
            conn,
            queue,
            app,
            width: w,
            height: h,
            color,
        }
    }

    /// Pump this client and assert it composited an EXACT solid-`color` frame — proof the whole
    /// wl → scene → present path still serves a normal client after abuse. Returns the captured frame.
    pub fn assert_presents(&mut self, captures: &Arc<Mutex<Vec<CapturedFrame>>>) -> CapturedFrame {
        let (w, h, color) = (self.width, self.height, self.color);
        let frame = pump_until(&mut self.queue, &mut self.app, captures, 5, move |f| {
            f.width == w && f.height == h && f.pixel_is(1, 1, color)
        })
        .expect("neighbor frame never composited after abuse (adapter did not survive)");
        // Exact solid fill: center + all four corners are the neighbor's color, nothing smeared.
        for (x, y) in [
            (w / 2, h / 2),
            (0, 0),
            (w - 1, 0),
            (0, h - 1),
            (w - 1, h - 1),
        ] {
            assert_eq!(
                frame.pixel(x, y).unwrap(),
                color,
                "neighbor pixel ({x},{y}) is its solid color"
            );
        }
        frame
    }

    /// Roundtrip the client's queue once (drain server events, flush requests).
    pub fn pump(&mut self) {
        let _ = self.queue.roundtrip(&mut self.app);
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for NeighborApp {
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
impl Dispatch<XdgWmBase, ()> for NeighborApp {
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
impl Dispatch<XdgSurface, ()> for NeighborApp {
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
                app.surface.damage(0, 0, i32::MAX, i32::MAX);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for NeighborApp {
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
macro_rules! neighbor_ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for NeighborApp {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
neighbor_ignore!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    XdgToplevel
);
