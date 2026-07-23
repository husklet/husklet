use super::*;

/// A connected `wl_shm` present session to a wayland compositor.
pub struct Wayland {
    pub(super) fd: c_int,
    pub(super) tx: Vec<u8>,
    pub(super) rx: Vec<u8>,
    pub(super) ready: bool,
    pub(super) globals: Vec<RegistryGlobal>,
    pub(super) sync_done: bool,
    pub(super) configure_serial: Option<u32>,
    pub(super) frame_done: bool,
}

impl Wayland {
    /// Append one wayland request: `[obj][ (size<<16)|op ][words…]`.
    pub(super) fn wmsg(&mut self, obj: u32, op: u16, words: &[u32]) {
        let sz = (8 + words.len() * 4) as u32;
        self.tx.extend_from_slice(&obj.to_le_bytes());
        self.tx
            .extend_from_slice(&((sz << 16) | op as u32).to_le_bytes());
        for w in words {
            self.tx.extend_from_slice(&w.to_le_bytes());
        }
    }

    /// A `wl_registry.bind(name, interface, version, new_id)` — `name` is the DISCOVERED registry name.
    pub(super) fn bind(&mut self, name: u32, interface: &str, version: u32, new_id: u32) {
        let mut words = vec![name, (interface.len() + 1) as u32];
        let mut sbuf = interface.as_bytes().to_vec();
        sbuf.push(0);
        while !sbuf.len().is_multiple_of(4) {
            sbuf.push(0);
        }
        for chunk in sbuf.chunks(4) {
            let mut wbuf = [0u8; 4];
            wbuf[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(wbuf));
        }
        words.push(version);
        words.push(new_id);
        self.wmsg(OBJ_REGISTRY, 0, &words);
    }

    /// Bind a required interface by its advertised name (clamped to the version we speak). Returns false
    /// if the compositor never advertised it.
    pub(super) fn bind_discovered(
        &mut self,
        interface: &str,
        max_version: u32,
        new_id: u32,
    ) -> bool {
        let Some(g) = self
            .globals
            .iter()
            .find(|g| g.interface == interface)
            .cloned()
        else {
            return false;
        };
        let version = g.version.min(max_version).max(1);
        self.bind(g.name, interface, version, new_id);
        true
    }

    /// Full-write the pending buffer, propagating a short write / disconnect.
    pub(super) fn wflush(&mut self) -> WlResult<()> {
        let mut sent = 0usize;
        while sent < self.tx.len() {
            let n = unsafe {
                write(
                    self.fd,
                    self.tx[sent..].as_ptr() as *const c_void,
                    self.tx.len() - sent,
                )
            };
            if n < 0 {
                self.tx.clear();
                return Err(WlError::Disconnected);
            }
            if n == 0 {
                self.tx.clear();
                return Err(WlError::ShortWrite);
            }
            sent += n as usize;
        }
        self.tx.clear();
        Ok(())
    }

    /// Flush the pending buffer with a single fd attached via `SCM_RIGHTS`, propagating failure.
    pub(super) fn wflush_fd(&mut self, fd: c_int) -> WlResult<()> {
        let want = self.tx.len();
        let mut iov = IoVec {
            base: self.tx.as_mut_ptr() as *mut c_void,
            len: want,
        };
        let mut cbuf = [0u8; 24]; // CMSG_SPACE(sizeof(int)) == 24 on LP64
        cbuf[0..8].copy_from_slice(&20usize.to_ne_bytes()); // cmsg_len = CMSG_LEN(4) = 20
        cbuf[8..12].copy_from_slice(&SOL_SOCKET.to_ne_bytes());
        cbuf[12..16].copy_from_slice(&SCM_RIGHTS.to_ne_bytes());
        cbuf[16..20].copy_from_slice(&fd.to_ne_bytes());
        let mh = MsgHdr {
            name: core::ptr::null_mut(),
            namelen: 0,
            _pad0: 0,
            iov: &mut iov,
            iovlen: 1,
            control: cbuf.as_mut_ptr() as *mut c_void,
            controllen: cbuf.len(),
            flags: 0,
        };
        let n = unsafe { sendmsg(self.fd, &mh, 0) };
        self.tx.clear();
        if n < 0 {
            return Err(WlError::FdSend);
        }
        if (n as usize) < want {
            return Err(WlError::ShortWrite);
        }
        Ok(())
    }

    pub(super) fn send_geometry(&mut self, g: &Geometry) {
        if g.should_send() {
            self.wmsg(
                OBJ_XDG_SURFACE,
                3,
                &[
                    g.geom_x as u32,
                    g.geom_y as u32,
                    g.logical_w as u32,
                    g.logical_h as u32,
                ],
            );
        }
    }

    /// Poll + read one batch of compositor events into `rx`. `Ok(true)` if bytes were read, `Ok(false)` on
    /// a poll timeout, `Err(Disconnected)` on EOF / socket error (a closed peer is a real failure).
    pub(super) fn pump(&mut self, timeout_ms: i32) -> WlResult<bool> {
        if self.fd < 0 {
            return Ok(false);
        }
        let mut pfd = PollFd {
            fd: self.fd,
            events: POLLIN,
            revents: 0,
        };
        let pr = unsafe { poll(&mut pfd, 1, timeout_ms) };
        if pr < 0 {
            return Err(WlError::Disconnected);
        }
        if pr == 0 {
            return Ok(false);
        }
        if pfd.revents & POLLIN == 0 {
            return Err(WlError::Disconnected);
        }
        let mut buf = [0u8; 8192];
        let n = unsafe { read(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n <= 0 {
            return Err(WlError::Disconnected);
        }
        self.rx.extend_from_slice(&buf[..n as usize]);
        Ok(true)
    }

    /// Process every complete message currently buffered in `rx`: records `wl_registry.global`, marks
    /// sync/frame callbacks done, captures the configure serial, answers `xdg_wm_base.ping` with a pong,
    /// and turns `wl_display.error` into [`WlError::Protocol`].
    pub(super) fn dispatch_pending(&mut self) -> WlResult<()> {
        let mut off = 0usize;
        let mut pong: Option<u32> = None;
        while self.rx.len() - off >= 8 {
            let obj = u32::from_le_bytes(self.rx[off..off + 4].try_into().unwrap());
            let so = u32::from_le_bytes(self.rx[off + 4..off + 8].try_into().unwrap());
            let size = (so >> 16) as usize;
            let op = (so & 0xffff) as u16;
            if size < 8 || self.rx.len() - off < size {
                break;
            }
            let body = &self.rx[off + 8..off + size];
            match (obj, op) {
                (OBJ_DISPLAY, 0) if body.len() >= 8 => {
                    let object = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    let code = u32::from_le_bytes(body[4..8].try_into().unwrap());
                    self.rx.drain(..off + size);
                    return Err(WlError::Protocol { object, code });
                }
                (OBJ_REGISTRY, 0) => {
                    if let Some((name, interface, version)) = RegistryEvent::parse(body) {
                        self.globals.push(RegistryGlobal {
                            name,
                            interface,
                            version,
                        });
                    }
                }
                (OBJ_SYNC_CB, 0) => self.sync_done = true,
                (OBJ_FRAME_CB, 0) => self.frame_done = true,
                (OBJ_XDG_SURFACE, 0) if body.len() >= 4 => {
                    self.configure_serial =
                        Some(u32::from_le_bytes(body[0..4].try_into().unwrap()));
                }
                (OBJ_XDG_WM_BASE, 0) if body.len() >= 4 => {
                    pong = Some(u32::from_le_bytes(body[0..4].try_into().unwrap()));
                }
                _ => {}
            }
            off += size;
        }
        if off > 0 {
            self.rx.drain(..off);
        }
        if let Some(serial) = pong {
            self.wmsg(OBJ_XDG_WM_BASE, 3, &[serial]); // xdg_wm_base.pong
            self.wflush()?;
        }
        Ok(())
    }

    /// Send `get_registry` + a `wl_display.sync` barrier, read until the sync callback returns, then bind
    /// each required interface by its discovered name.
    pub(super) fn discover_and_bind(&mut self) -> WlResult<()> {
        self.wmsg(OBJ_DISPLAY, 1, &[OBJ_REGISTRY]); // wl_display.get_registry
        self.wmsg(OBJ_DISPLAY, 0, &[OBJ_SYNC_CB]); // wl_display.sync — end-of-globals barrier
        self.wflush()?;
        let deadline = now_ms() + HANDSHAKE_DEADLINE_MS;
        while !self.sync_done {
            self.dispatch_pending()?;
            if self.sync_done {
                break;
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                break;
            }
            if !self.pump(rem as c_int + 1)? {
                break;
            }
        }
        if !self.bind_discovered("wl_compositor", 4, OBJ_COMPOSITOR)
            || !self.bind_discovered("wl_shm", 1, OBJ_SHM)
            || !self.bind_discovered("xdg_wm_base", 1, OBJ_XDG_WM_BASE)
        {
            return Err(WlError::MissingGlobal);
        }
        Ok(())
    }

    /// After creating the surface/xdg_surface/toplevel, wait for `configure` and ack it with the RECEIVED
    /// serial (never an invented constant).
    pub(super) fn ack_first_configure(&mut self) -> WlResult<()> {
        let deadline = now_ms() + HANDSHAKE_DEADLINE_MS;
        while self.configure_serial.is_none() {
            self.dispatch_pending()?;
            if self.configure_serial.is_some() {
                break;
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                break;
            }
            if !self.pump(rem as c_int + 1)? {
                break;
            }
        }
        if let Some(serial) = self.configure_serial {
            self.wmsg(OBJ_XDG_SURFACE, 4, &[serial]); // xdg_surface.ack_configure
            self.wflush()?;
        }
        Ok(())
    }

    /// Connect to `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` and run discovery + surface bring-up. Returns None
    /// if the socket is unavailable or the handshake fails (an honest "no compositor" — never a fake up).
    pub fn connect_and_handshake(g: &Geometry) -> Option<Wayland> {
        let disp = std::env::var("WAYLAND_DISPLAY").ok()?;
        let rd = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/0".to_string());
        let path = if disp.starts_with('/') {
            disp
        } else {
            format!("{rd}/{disp}")
        };

        let fd = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
        if fd < 0 {
            return None;
        }
        let mut sa = [0u8; 110]; // sockaddr_un: family (u16) + path (108 bytes)
        sa[0..2].copy_from_slice(&(AF_UNIX as u16).to_ne_bytes());
        let pb = path.as_bytes();
        let n = pb.len().min(107);
        sa[2..2 + n].copy_from_slice(&pb[..n]);
        if unsafe { connect(fd, sa.as_ptr() as *const c_void, sa.len() as u32) } != 0 {
            unsafe { close(fd) };
            return None;
        }
        let mut w = Wayland {
            fd,
            tx: Vec::new(),
            rx: Vec::new(),
            ready: false,
            globals: Vec::new(),
            sync_done: false,
            configure_serial: None,
            frame_done: false,
        };
        if w.discover_and_bind().is_err() {
            return None;
        }
        w.wmsg(OBJ_COMPOSITOR, 0, &[OBJ_WL_SURFACE]); // wl_compositor.create_surface
        w.wmsg(OBJ_XDG_WM_BASE, 2, &[OBJ_XDG_SURFACE, OBJ_WL_SURFACE]); // xdg_wm_base.get_xdg_surface
        w.wmsg(OBJ_XDG_SURFACE, 1, &[OBJ_TOPLEVEL]); // xdg_surface.get_toplevel
        w.send_geometry(g);
        w.wmsg(OBJ_WL_SURFACE, 6, &[]); // wl_surface.commit (initial)
        if w.wflush().is_err() {
            return None;
        }
        if w.ack_first_configure().is_err() {
            return None;
        }
        w.ready = true;
        Some(w)
    }

    /// Commit one frame's `xrgb` pixels (`WL_SHM_FORMAT_XRGB8888`, top-left, tight `w*h*4`) as a `wl_shm`
    /// `wl_buffer` to the surface, then pace on the frame callback. Returns a typed error on any
    /// map/delivery/protocol/pacing failure — never a silent "presented".
    pub fn commit(&mut self, xrgb: &[u8], g: &Geometry) -> WlResult<()> {
        if !self.ready {
            return Err(WlError::Disconnected);
        }
        let (w, h) = (g.backing_w.max(1), g.backing_h.max(1));
        let stride = w * 4;
        let size = (stride * h) as usize;
        if xrgb.len() < size {
            return Err(WlError::ShmAlloc);
        }
        let shm = ShmBuffer::new(&xrgb[..size])?;
        self.frame_done = false;

        // wl_shm.create_pool(new_id=pool, fd, size) — the fd rides SCM_RIGHTS on the flush.
        self.wmsg(OBJ_SHM, 0, &[OBJ_SHM_POOL, size as u32]);
        self.wflush_fd(shm.fd)?;
        // wl_shm_pool.create_buffer(new_id=buffer, offset=0, width, height, stride, format).
        self.wmsg(
            OBJ_SHM_POOL,
            0,
            &[OBJ_WL_BUFFER, 0, w, h, stride, WL_SHM_FORMAT_XRGB8888],
        );
        self.wmsg(
            OBJ_WL_SURFACE,
            1,
            &[OBJ_WL_BUFFER, g.attach_x as u32, g.attach_y as u32],
        ); // attach
        self.wmsg(OBJ_WL_SURFACE, 2, &[0, 0, w, h]); // damage
        self.send_geometry(g);
        self.wmsg(OBJ_WL_SURFACE, 3, &[OBJ_FRAME_CB]); // frame(callback)
        self.wmsg(OBJ_WL_SURFACE, 6, &[]); // commit
        self.wflush()?;
        self.wmsg(OBJ_WL_BUFFER, 0, &[]); // wl_buffer.destroy (the compositor keeps its own dup)
        self.wmsg(OBJ_SHM_POOL, 1, &[]); // wl_shm_pool.destroy
        self.wflush()?;
        self.await_frame()
    }

    /// Drain events until the frame callback fires, bounded by the pacing deadline.
    pub(super) fn await_frame(&mut self) -> WlResult<()> {
        let deadline = now_ms() + FRAME_DEADLINE_MS;
        loop {
            self.dispatch_pending()?;
            if self.frame_done {
                return Ok(());
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                return Err(WlError::FrameTimeout);
            }
            if !self.pump(rem as c_int + 1)? {
                continue;
            }
        }
    }
}

impl Drop for Wayland {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { close(self.fd) };
        }
    }
}
