use super::*;
use hl_gl::model::context::IrAllocator;

fn attributes() -> ContextAttributes {
    ContextAttributes::default()
}

#[test]
fn context_zero_never_resolves_to_a_share_group() {
    let state = State::new();
    assert!(state.contexts.group(0).is_none());
}

#[test]
fn unbound_surface_carries_target_between_unshared_contexts() {
    let mut state = State::new();
    state.inited = true;
    state.create_context(1, attributes());
    state.create_context(2, attributes());
    let surface = state.create_surface(SurfaceKind::Window, 640, 480, 0, 0) as usize;
    let slot = state.surface_slot(surface).expect("surface");

    let first = state.contexts.group(1).expect("first group");
    let target = {
        let mut lease = first.acquire().expect("first");
        assert!(lease.data_mut().activate(1));
        lease.data_mut().gl.bind_surfaces(
            surface as u64,
            GlSurface {
                have: true,
                width: 640,
                height: 480,
            },
            SurfaceKind::Window,
            surface as u64,
            GlSurface {
                have: true,
                width: 640,
                height: 480,
            },
            SurfaceKind::Window,
        );
        let (_, texture, created) = lease
            .data_mut()
            .gl
            .default_target(640, 480)
            .expect("target");
        assert!(created);
        let target = lease.data_mut().gl.take_surface_target(surface as u64);
        assert!(!target.is_empty());
        assert_eq!(texture, 1);
        target
    };
    slot.install_target(target);

    let second = state.contexts.group(2).expect("second group");
    let mut lease = second.acquire().expect("second");
    assert!(lease.data_mut().activate(2));
    lease
        .data_mut()
        .gl
        .install_surface_target(surface as u64, slot.take_target());
    lease.data_mut().gl.bind_surfaces(
        surface as u64,
        GlSurface {
            have: true,
            width: 640,
            height: 480,
        },
        SurfaceKind::Window,
        surface as u64,
        GlSurface {
            have: true,
            width: 640,
            height: 480,
        },
        SurfaceKind::Window,
    );
    assert_eq!(
        lease.data().gl.resident_default_read_target(),
        Some((
            1,
            640,
            480,
            hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm
        ))
    );
}

#[test]
fn destroying_unbound_surface_retires_its_executor_resources() {
    let mut state = State::new();
    state.inited = true;
    state.create_context(1, attributes());
    let surface = state.create_surface(SurfaceKind::Window, 32, 32, 0, 0) as usize;
    let group = state.contexts.group(1).expect("group");
    let target = {
        let mut lease = group.acquire().expect("group");
        assert!(lease.data_mut().activate(1));
        lease.data_mut().gl.bind_draw_surface(
            surface as u64,
            GlSurface {
                have: true,
                width: 32,
                height: 32,
            },
            SurfaceKind::Window,
        );
        lease.data_mut().gl.default_target(32, 32).expect("target");
        lease.data_mut().gl.take_surface_target(surface as u64)
    };
    state
        .surface_slot(surface)
        .expect("surface")
        .install_target(target);

    assert!(state.destroy_surface(surface));
    assert!(state.surfaces.get(&surface).is_none());
    assert!(matches!(
        state.surface_retirements.as_slice(),
        [hl_gpu::Cmd::DestroyTexture(1)]
    ));
}

#[test]
fn actor_panic_loses_every_queued_share_group() {
    let sequencer = crate::transport::Sequencer::spawn(RemoteCommandSink::new(
        "/tmp/hl-gl-state-panic-test-unused.sock",
    ))
    .expect("actor");
    let allocator = Arc::new(IrAllocator::new());
    let panicking_group = group::GroupSlot::new(Arc::clone(&allocator));
    let queued_group = group::GroupSlot::new(allocator);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (panic_tx, panic_rx) = std::sync::mpsc::channel();

    let first_guard = InFlightGroup::new(Arc::clone(&panicking_group));
    let first = sequencer
        .submit(Plan::new(move |_| -> hl_gpu::Result<()> {
            let _guard = first_guard;
            entered_tx.send(()).expect("entered");
            panic_rx.recv().expect("panic released");
            panic!("expected actor panic");
        }))
        .expect("first");
    entered_rx.recv().expect("first entered");

    let queued_guard = InFlightGroup::new(Arc::clone(&queued_group));
    let queued = sequencer
        .submit(Plan::new(move |_| {
            let _guard = queued_guard;
            Ok(())
        }))
        .expect("queued");
    panic_tx.send(()).expect("release panic");

    assert!(first.wait().is_err());
    assert!(queued.wait().is_err());
    assert!(panicking_group.acquire().is_err());
    assert!(queued_group.acquire().is_err());
}

#[test]
fn resizing_a_transferred_target_allocates_fresh_ids_and_retires_old_texture() {
    let allocator = Arc::new(hl_gl::model::context::IrAllocator::new());
    let group = group::GroupSlot::new(allocator);
    let mut lease = group.acquire().expect("group");
    assert!(lease
        .data_mut()
        .add(1, hl_gl::model::context::ContextState::default()));
    assert!(lease.data_mut().activate(1));
    lease.data_mut().gl.bind_draw_surface(
        9,
        GlSurface {
            have: true,
            width: 64,
            height: 64,
        },
        SurfaceKind::Window,
    );
    let (_, first, _) = lease.data_mut().gl.default_target(64, 64).expect("first");
    let target = lease.data_mut().gl.take_surface_target(9);
    lease.data_mut().gl.install_surface_target(9, target);
    let (_, second, created) = lease
        .data_mut()
        .gl
        .default_target(128, 96)
        .expect("resized");

    assert!(created);
    assert_ne!(first, second);
    assert!(lease
        .data()
        .gl
        .pending_destroys()
        .iter()
        .any(|command| matches!(command, hl_gpu::Cmd::DestroyTexture(id) if *id == first)));
}

/// A surface cannot be created against a display that was never initialized, or that was terminated.
///
/// EGL 1.4 §3.5 requires `EGL_NOT_INITIALIZED` of every surface-creation entry point. Nothing here read
/// the flag: `eglInitialize` set it, `eglTerminate` cleared it, and no caller consulted it, so a
/// terminated display went on handing out surfaces as though it were live. What it hands out is not
/// equivalent either — `terminate` clears `native_present`, so a surface created afterwards silently
/// takes the readback path instead of presenting zero-copy.
///
/// The check lives on `create_surface` rather than on the entry points because all six spellings —
/// window, pbuffer, and the four `eglCreatePlatform*` variants — pass through it, and copying one
/// condition into six places is how a claim ends up stated six times and re-derived none.
#[test]
fn a_surface_needs_a_display_that_is_initialized() {
    let mut state = State::new();
    assert!(!state.inited, "a fresh display is not yet initialized");

    let refused = state.create_surface(SurfaceKind::Window, 64, 64, 0, 0);
    assert!(
        refused.is_null(),
        "an uninitialized display must not back a surface"
    );
    assert_eq!(
        state.take_egl_error(),
        hl_gl::result::EGL_NOT_INITIALIZED,
        "the refusal must say the display is not initialized"
    );

    state.inited = true;
    let created = state.create_surface(SurfaceKind::Window, 64, 64, 0, 0);
    assert!(
        !created.is_null(),
        "an initialized display still creates surfaces"
    );

    // The sequence that actually happens: a live display, then eglTerminate, then a create.
    state.terminate();
    let after_terminate = state.create_surface(SurfaceKind::Window, 64, 64, 0, 0);
    assert!(
        after_terminate.is_null(),
        "a terminated display must not back a surface"
    );
    assert_eq!(
        state.take_egl_error(),
        hl_gl::result::EGL_NOT_INITIALIZED
    );
}

/// The precondition both surface and context creation ask, on its own.
///
/// Tested here rather than through `eglCreateContext` because `inited` is process-global and a test that
/// cleared it would race every other test in this suite that creates an EGL object — the same hazard the
/// `initialized_display` harness helper exists to remove. A local `State` starts uninitialized, which is
/// the state a real display is in before `eglInitialize` and after `eglTerminate`.
#[test]
fn an_uninitialized_display_backs_nothing_and_says_so() {
    let mut state = State::new();
    assert!(!state.inited, "a fresh display is not yet initialized");
    assert!(!state.require_initialized(), "and backs nothing");
    assert_eq!(
        state.take_egl_error(),
        hl_gl::result::EGL_NOT_INITIALIZED,
        "the refusal must name the display's state, not the caller's arguments"
    );

    state.inited = true;
    assert!(state.require_initialized());
    assert_eq!(
        state.take_egl_error(),
        hl_gl::result::EGL_SUCCESS,
        "a display that is initialized records no error"
    );

    state.terminate();
    assert!(
        !state.require_initialized(),
        "eglTerminate puts the display back where eglInitialize found it"
    );
}
