use super::global::state_ptr;
use super::*;

impl GlobalState {
    pub fn remove_context(token: usize) -> bool {
        let retire = {
            // SAFETY: see [`Self::access`].
            let mutex: &Mutex<State> = unsafe { &*state_ptr() };
            let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
            if !state.contexts.contains(token) {
                return false;
            }
            let retire = state.contexts.destroy(token);
            state.live_contexts = state.contexts.live();
            retire
        };
        if let Some(retire) = retire {
            if let Ok(mut group) = retire.group.acquire() {
                group.data_mut().remove(retire.token);
                if retire.final_context {
                    group.data_mut().retire();
                }
            }
        }
        true
    }

    pub fn bind_current(
        previous: (usize, usize, usize),
        next: (usize, usize, usize),
    ) -> Result<(), MakeCurrentError> {
        let binding = {
            // SAFETY: see [`Self::access`].
            let mutex: &Mutex<State> = unsafe { &*state_ptr() };
            let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
            state.make_current(previous, next)?
        };
        let same_group = binding
            .previous_group
            .as_ref()
            .zip(binding.group.as_ref())
            .is_some_and(|(previous, next)| Arc::ptr_eq(previous, next));
        let mut surface_retirements = Vec::new();
        if same_group {
            let group = binding
                .group
                .as_ref()
                .expect("same-group binding has next group");
            let mut lease = group.acquire().map_err(|_| MakeCurrentError::Context)?;
            surface_retirements.extend(Self::release_bound_surfaces(
                lease.data_mut(),
                binding.previous_context,
                binding.previous_draw.as_ref(),
                binding.previous_read.as_ref(),
            )?);
            Self::install_bound_surfaces(
                lease.data_mut(),
                binding.context,
                binding.draw.as_ref(),
                binding.read.as_ref(),
            )?;
        } else {
            if let Some(group) = binding.previous_group.as_ref() {
                let mut lease = group.acquire().map_err(|_| MakeCurrentError::Context)?;
                surface_retirements.extend(Self::release_bound_surfaces(
                    lease.data_mut(),
                    binding.previous_context,
                    binding.previous_draw.as_ref(),
                    binding.previous_read.as_ref(),
                )?);
            }
            if let Some(group) = binding.group.as_ref() {
                let mut lease = group.acquire().map_err(|_| MakeCurrentError::Context)?;
                Self::install_bound_surfaces(
                    lease.data_mut(),
                    binding.context,
                    binding.draw.as_ref(),
                    binding.read.as_ref(),
                )?;
            }
        }
        if let Some(retire) = binding.retire {
            if let Ok(mut group) = retire.group.acquire() {
                group.data_mut().remove(retire.token);
                if retire.final_context {
                    group.data_mut().retire();
                }
            }
        }
        if !surface_retirements.is_empty() {
            Self::submit_surface_retirements(surface_retirements)
                .map_err(|_| MakeCurrentError::Surface)?;
        }
        Ok(())
    }

    fn release_bound_surfaces(
        data: &mut group::GroupData,
        context: usize,
        draw: Option<&BoundSurface>,
        read: Option<&BoundSurface>,
    ) -> Result<Vec<hl_gpu::Cmd>, MakeCurrentError> {
        let mut retirements = Vec::new();
        if context == 0 || !data.activate(context) {
            return Ok(retirements);
        }
        if let Some(draw) = draw {
            let target = data.gl.take_surface_target(draw.token as u64);
            if draw.live {
                draw.slot.install_target(target);
            } else {
                retirements.extend(target.retire());
            }
        }
        if let Some(read) = read.filter(|read| draw.is_none_or(|draw| draw.token != read.token)) {
            let target = data.gl.take_surface_target(read.token as u64);
            if read.live {
                read.slot.install_target(target);
            } else {
                retirements.extend(target.retire());
            }
        }
        Ok(retirements)
    }

    fn install_bound_surfaces(
        data: &mut group::GroupData,
        context: usize,
        draw: Option<&BoundSurface>,
        read: Option<&BoundSurface>,
    ) -> Result<(), MakeCurrentError> {
        if context == 0 {
            return Ok(());
        }
        if !data.activate(context) {
            return Err(MakeCurrentError::Context);
        }
        if let Some(draw) = draw {
            data.gl
                .install_surface_target(draw.token as u64, draw.slot.take_target());
        }
        if let Some(read) = read.filter(|read| draw.is_none_or(|draw| draw.token != read.token)) {
            data.gl
                .install_surface_target(read.token as u64, read.slot.take_target());
        }
        let draw_info = draw
            .map(|surface| (surface.info.render, surface.info.kind))
            .unwrap_or((GlSurface::default(), SurfaceKind::Offscreen));
        let read_info = read
            .map(|surface| (surface.info.render, surface.info.kind))
            .unwrap_or((GlSurface::default(), SurfaceKind::Offscreen));
        data.gl.bind_surfaces(
            draw.map_or(0, |surface| surface.token as u64),
            draw_info.0,
            draw_info.1,
            read.map_or(0, |surface| surface.token as u64),
            read_info.0,
            read_info.1,
        );
        Ok(())
    }

    pub fn register_context(
        token: usize,
        attributes: ContextAttributes,
        share: Option<usize>,
    ) -> bool {
        let prepared = {
            // SAFETY: see [`Self::access`].
            let mutex: &Mutex<State> = unsafe { &*state_ptr() };
            let state = mutex.lock().unwrap_or_else(|error| error.into_inner());
            state.contexts.prepare(attributes, share)
        };
        let Some(prepared) = prepared else {
            return false;
        };
        let Ok(mut group) = prepared.group.acquire() else {
            return false;
        };
        if !group.data_mut().add(token, prepared.state) {
            return false;
        }
        let committed = {
            // SAFETY: see [`Self::access`].
            let mutex: &Mutex<State> = unsafe { &*state_ptr() };
            let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
            let share_is_live = share.is_none_or(|shared| {
                state.contexts.contains(shared) && state.contexts.shares(shared, &prepared.group)
            });
            let committed = share_is_live
                && state
                    .contexts
                    .commit(token, attributes, Arc::clone(&prepared.group));
            state.live_contexts = state.contexts.live();
            committed
        };
        if !committed {
            group.data_mut().remove(token);
        }
        committed
    }

    pub fn access<R>(f: impl FnOnce(&mut State) -> R) -> R {
        // The driver's logging composition root: opens `hl-log`'s runtime tag mask from the environment
        // on first use, so an `hl_error!` in an entry point can actually reach stderr. One relaxed
        // atomic after the first call. See [`crate::logging::GuestLogging`].
        crate::logging::GuestLogging::install();
        // SAFETY: `state_ptr` returns a `&'static Mutex<State>` (as a raw pointer) that is either the owner's
        // own `OnceLock`-backed cell or the same cell imported from libEGL — never null, never dangling.
        let m: &Mutex<State> = unsafe { &*state_ptr() };
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        let result = f(&mut g);
        result
    }

    /// Run one GL operation against the calling thread's share group.
    ///
    /// The process registry is released before waiting for a busy group. Unrelated share groups therefore
    /// remain usable while one context lowers work or waits for its own group.
    pub(crate) fn context<R: Default>(f: impl FnOnce(&mut group::GroupData) -> R) -> R {
        Self::context_for(current::context(), f)
    }

    pub(crate) fn context_for<R: Default>(
        token: usize,
        f: impl FnOnce(&mut group::GroupData) -> R,
    ) -> R {
        let Some(group) = Self::group(token) else {
            return R::default();
        };
        let mut lease = match group.acquire() {
            Ok(lease) => lease,
            Err(_) => return R::default(),
        };
        let data = lease.data_mut();
        if !data.activate(token) {
            data.gl.set_gl_error(hl_gl::result::GL_INVALID_OPERATION);
        }
        let result = f(data);
        data.collect_deleted_objects();
        result
    }
}
