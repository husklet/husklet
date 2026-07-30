use super::*;

impl State {
    pub(super) fn new() -> Self {
        let endpoint = std::env::var("HL_GPU_EXEC");
        let remote_required = endpoint.is_ok();
        let gpu_endpoint = endpoint.unwrap_or_else(|_| DEFAULT_EXEC_SOCK.to_owned());
        let mut sink =
            RemoteCommandSink::with_config(gpu_endpoint.clone(), TransportConfig::default());
        sink.set_trace(
            std::env::var_os("HL_GPU_TRACE").is_some()
                || std::env::var_os("HL_SHIM_DEBUG").is_some(),
        );
        let allocator = Arc::new(hl_gl::model::context::IrAllocator::new());
        State {
            inited: false,
            contexts: Contexts::new(allocator),
            transport: Arc::new(DisplayTransport::new(sink)),
            ready: None,
            // Explicit projections connect at EGL initialization; tests without one retain lazy connect.
            remote_required,
            next_token: 1,
            surfaces: HashMap::new(),
            surface_current: HashMap::new(),
            pending_surfaces: HashSet::new(),
            surface_retirements: Vec::new(),
            images: Images::default(),
            // Tests without a projected executor use the real WGPU backend's conservative ceiling.
            max_buffer_bytes: 256 << 20,
            next_external_serial: Some(1),
            current_is_wayland: false,
            wl_surface_ptr: 0,
            current_surface: 0,
            wl: None,
            native_present: false,
            live_contexts: 0,
        }
    }

    /// Establish an explicitly configured host GPU capability before the application locks down its
    /// syscall surface. Host-side ABI tests without a projected endpoint retain the lazy test seam.
    pub fn initialize(&mut self) -> std::io::Result<()> {
        self.initialize_with(|sink, request| sink.negotiate(request))
    }

    fn initialize_with(
        &mut self,
        negotiate: impl FnOnce(
            &mut RemoteCommandSink,
            &FeatureRequest,
        ) -> hl_gpu::Result<hl_gpu::Capabilities>,
    ) -> std::io::Result<()> {
        if self.inited {
            return Ok(());
        }
        if self.remote_required {
            let request = FeatureRequest {
                wire_version: WIRE_VERSION,
                ..FeatureRequest::default()
            };
            let ready = self
                .transport
                .ensure_with(&request, negotiate)
                .map_err(std::io::Error::other)?;
            let capabilities = &ready.capabilities;
            self.native_present = capabilities.present_kinds.contains(&PresentKind::IoSurface);
            self.max_buffer_bytes = capabilities.max_buffer_bytes;
            if std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some() {
                eprintln!(
                    "capture ownership boundary=egl_negotiate present_kinds={:?} native={} max_buffer_bytes={}",
                    capabilities.present_kinds,
                    self.native_present,
                    self.max_buffer_bytes
                );
            }
            self.ready = Some(ready);
        }
        self.inited = true;
        Ok(())
    }

    pub fn external_buffers_enabled(&self) -> bool {
        self.native_present
    }

    pub fn terminate(&mut self) {
        self.inited = false;
        self.native_present = false;
    }

    pub fn reserve_external_serial(
        &mut self,
    ) -> hl_gpu::Result<hl_gpu::protocol::model::descriptor::FrameSerial> {
        let candidate = self
            .next_external_serial
            .ok_or(hl_gpu::GpuError::ResourceLimit("external frame serials"))?;
        self.next_external_serial = candidate.checked_add(1);
        hl_gpu::protocol::model::descriptor::FrameSerial::new(candidate)
    }

    /// Create an EGL context with a new, isolated share group.
    pub fn create_context(&mut self, token: usize, attributes: ContextAttributes) {
        let prepared = self
            .contexts
            .prepare(attributes, None)
            .expect("new context has a group");
        let mut group = prepared.group.acquire().expect("new group is live");
        let created = group.data_mut().add(token, prepared.state)
            && self
                .contexts
                .commit(token, attributes, Arc::clone(&prepared.group));
        debug_assert!(created);
        self.live_contexts = self.contexts.live();
    }

    /// Create an EGL context that joins `share`'s object namespace.
    pub fn create_shared_context(
        &mut self,
        token: usize,
        attributes: ContextAttributes,
        share: usize,
    ) -> bool {
        let Some(prepared) = self.contexts.prepare(attributes, Some(share)) else {
            return false;
        };
        let Ok(mut group) = prepared.group.acquire() else {
            return false;
        };
        let created = group.data_mut().add(token, prepared.state)
            && self
                .contexts
                .commit(token, attributes, Arc::clone(&prepared.group));
        self.live_contexts = self.contexts.live();
        created
    }

    pub fn context_attributes(&self, token: usize) -> Option<ContextAttributes> {
        self.contexts.attributes(token)
    }

    pub fn has_context(&self, token: usize) -> bool {
        self.contexts.contains(token)
    }

    pub fn context_is_current(&self, token: usize) -> bool {
        self.contexts.is_current(token)
    }

    pub fn current_share_group(&self) -> Option<usize> {
        self.contexts
            .group(current::context())
            .map(|group| Arc::as_ptr(&group) as usize)
    }

    fn bind_context(
        &mut self,
        previous: usize,
        next: usize,
    ) -> Result<Option<context::Retire>, ContextBindError> {
        let retire = self.contexts.bind(previous, next)?;
        self.live_contexts = self.contexts.live();
        Ok(retire)
    }

    pub fn create_sync(&mut self, context: usize, gl: usize) -> usize {
        let token = self.mint_token() as usize;
        self.contexts.create_sync(token, EglSync { context, gl });
        token
    }

    pub fn sync(&self, token: usize) -> Option<EglSync> {
        self.contexts.sync(token)
    }

    pub fn destroy_sync(&mut self, token: usize) -> Option<EglSync> {
        self.contexts.destroy_sync(token)
    }

    pub fn create_surface(
        &mut self,
        kind: SurfaceKind,
        width: u32,
        height: u32,
        wl_surface: usize,
        native_window: usize,
    ) -> *mut c_void {
        let token = self.mint_token() as usize;
        let surface = Surface {
            render: GlSurface {
                have: true,
                width,
                height,
            },
            kind,
            wl_surface,
            native_window,
            wl_app: None,
            wl_app_unavailable: false,
            target: SurfaceTarget::default(),
        };
        self.surfaces
            .insert(token, Arc::new(SurfaceSlot::new(surface)));
        token as *mut c_void
    }

    pub fn surface_slot(&self, token: usize) -> Option<Arc<SurfaceSlot>> {
        self.surfaces.get(&token).cloned()
    }

    pub fn surface(&self, token: usize) -> Option<SurfaceInfo> {
        self.surface_slot(token).map(|surface| surface.info())
    }

    pub fn has_surface(&self, token: usize) -> bool {
        self.surfaces.contains_key(&token) && !self.pending_surfaces.contains(&token)
    }

    pub fn destroy_surface(&mut self, token: usize) -> bool {
        if !self.has_surface(token) {
            return false;
        }
        if self.surface_current.get(&token).copied().unwrap_or(0) != 0 {
            self.pending_surfaces.insert(token);
            return true;
        }
        self.destroy_surface_now(token);
        true
    }

    fn destroy_surface_now(&mut self, token: usize) {
        self.pending_surfaces.remove(&token);
        self.surface_current.remove(&token);
        if let Some(surface) = self.surfaces.remove(&token) {
            self.surface_retirements
                .extend(surface.take_target().retire());
        }
        if self.current_surface == token {
            self.current_surface = 0;
        }
        if self.wl_surface_ptr != 0
            && self
                .surfaces
                .values()
                .all(|surface| surface.info().wl_surface != self.wl_surface_ptr)
        {
            self.wl_surface_ptr = 0;
            self.current_is_wayland = false;
        }
    }

    fn binding_surfaces(draw: usize, read: usize) -> impl Iterator<Item = usize> {
        [draw, read]
            .into_iter()
            .filter(|token| *token != 0)
            .enumerate()
            .filter_map(move |(index, token)| (index == 0 || token != draw).then_some(token))
    }

    fn surface_is_current(&self, token: usize) -> bool {
        self.surface_current.get(&token).copied().unwrap_or(0) != 0
    }

    pub(super) fn make_current(
        &mut self,
        previous: (usize, usize, usize),
        next: (usize, usize, usize),
    ) -> Result<Binding, MakeCurrentError> {
        let (previous_context, previous_draw, previous_read) = previous;
        let (next_context, next_draw, next_read) = next;
        let previous_group = self.contexts.group(previous_context);
        let previous_draw_slot = self.surface_slot(previous_draw);
        let previous_read_slot = self.surface_slot(previous_read);

        if next_context != 0 {
            if !self.has_context(next_context) {
                return Err(MakeCurrentError::Context);
            }
            if next_context != previous_context && self.context_is_current(next_context) {
                return Err(MakeCurrentError::Access);
            }
            for surface in Self::binding_surfaces(next_draw, next_read) {
                if !self.has_surface(surface) {
                    return Err(MakeCurrentError::Surface);
                }
                if !Self::binding_surfaces(previous_draw, previous_read)
                    .any(|current| current == surface)
                    && self.surface_is_current(surface)
                {
                    return Err(MakeCurrentError::Access);
                }
            }
        }

        let retire =
            self.bind_context(previous_context, next_context)
                .map_err(|error| match error {
                    ContextBindError::Missing => MakeCurrentError::Context,
                    ContextBindError::Current => MakeCurrentError::Access,
                })?;

        for surface in Self::binding_surfaces(previous_draw, previous_read) {
            if let Some(count) = self.surface_current.get_mut(&surface) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.surface_current.remove(&surface);
                    if self.pending_surfaces.contains(&surface) {
                        self.destroy_surface_now(surface);
                    }
                }
            }
        }
        for surface in Self::binding_surfaces(next_draw, next_read) {
            *self.surface_current.entry(surface).or_default() += 1;
        }

        Ok(Binding {
            previous_context,
            previous_group,
            previous_draw: previous_draw_slot.map(|slot| BoundSurface {
                token: previous_draw,
                info: slot.info(),
                live: self.surfaces.contains_key(&previous_draw),
                slot,
            }),
            previous_read: previous_read_slot.map(|slot| BoundSurface {
                token: previous_read,
                info: slot.info(),
                live: self.surfaces.contains_key(&previous_read),
                slot,
            }),
            context: next_context,
            group: self.contexts.group(next_context),
            draw: self.surface_slot(next_draw).map(|slot| BoundSurface {
                token: next_draw,
                info: slot.info(),
                live: true,
                slot,
            }),
            read: self.surface_slot(next_read).map(|slot| BoundSurface {
                token: next_read,
                info: slot.info(),
                live: true,
                slot,
            }),
            retire,
        })
    }

    pub fn refresh_surface(&mut self, token: usize) {
        let Some((native_window, kind, old_width, old_height)) =
            self.surface(token).map(|surface| {
                (
                    surface.native_window,
                    surface.kind,
                    surface.render.width,
                    surface.render.height,
                )
            })
        else {
            return;
        };
        if kind != SurfaceKind::Window || native_window == 0 {
            return;
        }
        // SAFETY: the pointer is the live native window retained by the EGL surface.
        let info =
            unsafe { hl_gl::adapter::wayland::WlWindowInfo::parse(native_window as *mut c_void) };
        if info.width == old_width && info.height == old_height {
            return;
        }
        if let Some(surface) = self.surface_slot(token) {
            let mut surface = surface.state.lock().expect("surface slot poisoned");
            surface.render.width = info.width;
            surface.render.height = info.height;
        }
    }

    fn ensure_app_presenter(&mut self) -> bool {
        let Some(surface) = self.surface_slot(self.current_surface) else {
            return false;
        };
        let mut surface = surface.state.lock().expect("surface slot poisoned");
        if surface.wl_app.is_none() && !surface.wl_app_unavailable {
            // SAFETY: `wl_surface_ptr` came from the live wl_egl_window for the selected EGL surface.
            match unsafe {
                hl_gl::adapter::wayland_app::WaylandAppPresenter::new(surface.wl_surface)
            } {
                Ok(presenter) => surface.wl_app = Some(presenter),
                Err(_) => surface.wl_app_unavailable = true,
            }
        }
        surface.wl_app.is_some()
    }

    /// Remove one context and retire its private target. The final context also retires shared resources.
    pub fn destroy_context(&mut self, token: usize) {
        if let Some(retire) = self.contexts.destroy(token) {
            if let Ok(mut group) = retire.group.acquire() {
                group.data_mut().remove(retire.token);
                if retire.final_context {
                    group.data_mut().retire();
                }
            }
        }
        self.live_contexts = self.contexts.live();
    }

    /// Present the read-back frame onto the app's OWN `wl_surface`, lazily bringing up the presenter from
    /// [`Self::wl_surface_ptr`]. A soft (unavailable) bring-up latches [`Self::wl_app_unavailable`] so the
    /// dlopen is not retried each frame; a live commit failure is [`AppPresentOutcome::Failed`]. Never
    /// fakes a present.
    pub fn present_to_app_surface(&mut self, xrgb: &[u8], w: u32, h: u32) -> AppPresentOutcome {
        if !self.ensure_app_presenter() {
            return AppPresentOutcome::Unavailable;
        }
        let surface = self
            .surface_slot(self.current_surface)
            .expect("ensure_app_presenter retained surface");
        let mut surface = surface.state.lock().expect("surface slot poisoned");
        let presenter = surface
            .wl_app
            .as_mut()
            .expect("ensure_app_presenter installed presenter");
        match presenter.present(xrgb, w, h) {
            Ok(()) => AppPresentOutcome::Presented,
            // A soft error here (e.g. the connection died) still means "no live app present": treat as a
            // hard Failed only when it is genuinely a live-present failure, else fall back.
            Err(e) if e.is_unavailable() => {
                surface.wl_app = None;
                surface.wl_app_unavailable = true;
                AppPresentOutcome::Unavailable
            }
            Err(_) => AppPresentOutcome::Failed,
        }
    }

    pub fn reserve_native_frame(&mut self) -> Option<hl_gl::adapter::wayland_app::NativeFrame> {
        if !self.native_present || !self.ensure_app_presenter() {
            return None;
        }
        let surface = self.surface_slot(self.current_surface)?;
        let mut surface = surface.state.lock().expect("surface slot poisoned");
        surface.wl_app.as_mut()?.reserve_native_frame()
    }

    pub fn commit_native_frame(
        &mut self,
        frame: hl_gl::adapter::wayland_app::NativeFrame,
        w: u32,
        h: u32,
    ) -> bool {
        let Some(surface) = self.surface_slot(self.current_surface) else {
            return false;
        };
        let mut surface = surface.state.lock().expect("surface slot poisoned");
        let Some(presenter) = surface.wl_app.as_mut() else {
            return false;
        };
        if presenter.commit_native(frame, w, h).is_ok() {
            true
        } else {
            presenter.retire_native();
            false
        }
    }

    /// Mint a fresh opaque token (for `EGLContext` / `EGLSurface`).
    pub fn mint_token(&mut self) -> *mut c_void {
        let t = self.next_token;
        self.next_token += 1;
        t as *mut c_void
    }

    /// Record an EGL error for the CALLING THREAD (kept until `eglGetError` clears it). Per-thread so a
    /// present failure on one thread never surfaces as a lost context on another (see [`EGL_ERROR`]).
    pub fn set_egl_error(&mut self, e: i32) {
        EGL_ERROR.with(|c| c.set(e));
    }

    /// Read + clear the calling thread's last EGL error.
    pub fn take_egl_error(&mut self) -> i32 {
        EGL_ERROR.with(|c| c.replace(EGL_SUCCESS))
    }
}
