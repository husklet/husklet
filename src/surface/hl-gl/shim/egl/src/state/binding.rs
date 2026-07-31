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

    /// Terminate the calling thread's share group. Test-only: in production a group is lost by a GPU
    /// transport failure, which a unit test cannot provoke without a live actor.
    #[cfg(test)]
    pub(crate) fn lose_current_group(reason: &str) {
        if let Some(group) = Self::group(current::context()) {
            group.lose(reason.to_string());
        }
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
            let mut lease = group.acquire().map_err(|_| MakeCurrentError::Lost)?;
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
            // Releasing the OUTGOING binding is best-effort, exactly as `remove_context` already treats
            // the same lease. A terminated group has no state left to write a surface target back into,
            // so failing here would refuse `eglMakeCurrent(dpy, NULL, NULL, NULL)` — the first step of
            // the only recovery EGL defines — and leave the application bound to a dead context with no
            // way off it. `eglDestroyContext` tolerated this and make-current did not; the two are the
            // same condition and the strict one was the oversight.
            if let Some(group) = binding.previous_group.as_ref() {
                if let Ok(mut lease) = group.acquire() {
                    surface_retirements.extend(Self::release_bound_surfaces(
                        lease.data_mut(),
                        binding.previous_context,
                        binding.previous_draw.as_ref(),
                        binding.previous_read.as_ref(),
                    )?);
                }
            }
            if let Some(group) = binding.group.as_ref() {
                let mut lease = group.acquire().map_err(|_| MakeCurrentError::Lost)?;
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

    /// Take the one `GL_CONTEXT_LOST` owed to the calling thread's context, if its group has been lost
    /// and nobody has read it yet.
    ///
    /// This exists because [`Self::context_for`] cannot report it. Every entry point runs its work inside
    /// the share-group lease, so once the group is lost there is no lease to run in and the only thing
    /// left to return is `R::default()` — for `glGetError` that default is `0`, `GL_NO_ERROR`, and the
    /// driver claims perfect health for the rest of the process. Measured: a `glGenTextures` after a lost
    /// group left the application's name at `0`, it bound the reserved texture, uploaded seven mip levels
    /// into nothing and dereferenced a null pointer several calls later, with `glGetError` answering
    /// `GL_NO_ERROR` throughout.
    pub(crate) fn take_context_lost() -> bool {
        Self::group(current::context()).is_some_and(|group| group.take_lost_report())
    }

    /// Whether the calling thread's share group has been terminated.
    pub(crate) fn context_is_lost() -> bool {
        Self::group(current::context()).is_some_and(|group| group.is_lost())
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
