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

/// A host that REFUSES one request must fail that one request. Escalating a refusal to share-group loss
/// is the largest amplifier in this driver: a lost group makes every later GL call a no-op reporting
/// `GL_CONTEXT_LOST`, so one bad submission takes down every case behind it in the same process and the
/// first failure disappears behind a cascade that has nothing to do with it. The refused submission is
/// safe to continue from as far as the TRANSPORT is concerned — `runtime::submit` rejects a batch
/// atomically and the connection carried the answer.
///
/// This test asserts that and only that. It does NOT establish that both sides agree the batch did not
/// happen, which an earlier version of this comment claimed: the residency mirror it cited is byte
/// accounting, while the GL-object-to-IR-id caches are advanced optimistically at prepare time and are
/// not rolled back on a NACK. A Chrome session died of exactly that gap on 2026-08-01. The missing test
/// is named in `submit.rs`: a refusal followed by a succeeding submission that re-creates the same
/// objects, asserting the second does not reference an id the first one's rollback discarded.
#[test]
fn a_refused_request_fails_the_call_and_keeps_the_share_group() {
    let sequencer = crate::transport::Sequencer::spawn(RemoteCommandSink::new(
        "/tmp/hl-gl-state-refusal-test-unused.sock",
    ))
    .expect("actor");
    let group = group::GroupSlot::new(Arc::new(IrAllocator::new()));

    let ticket = sequencer
        .submit(Plan::new(GlobalState::attempt(
            Arc::clone(&group),
            move |_| -> hl_gpu::Result<()> {
                Err(hl_gpu::GpuError::Transport(
                    hl_gpu::TransportError::Rejected {
                        phase: hl_gpu::TransportPhase::Acknowledgement,
                        acknowledgement: 0,
                    },
                ))
            },
        )))
        .expect("queued");

    assert!(ticket.wait().is_err(), "the refused call itself fails");
    assert!(!group.is_lost(), "a refusal must not retire the share group");
    assert!(
        !group.take_lost_report(),
        "no GL_CONTEXT_LOST is owed for a call that merely failed"
    );
    assert!(
        group.acquire().is_ok(),
        "the next GL call must still be able to take the lease"
    );
}

/// The control, and the half that must NOT change: a transport that is gone leaves nothing recoverable
/// behind it, so the group is right to die. Without this pair the test above would pass just as well
/// against a driver that never loses a group at all.
#[test]
fn a_transport_that_is_gone_still_loses_the_share_group() {
    let sequencer = crate::transport::Sequencer::spawn(RemoteCommandSink::new(
        "/tmp/hl-gl-state-gone-test-unused.sock",
    ))
    .expect("actor");
    let group = group::GroupSlot::new(Arc::new(IrAllocator::new()));

    let ticket = sequencer
        .submit(Plan::new(GlobalState::attempt(
            Arc::clone(&group),
            move |_| -> hl_gpu::Result<()> {
                Err(hl_gpu::GpuError::Transport(
                    hl_gpu::TransportError::Unavailable {
                        phase: hl_gpu::TransportPhase::FrameWrite,
                        detail: "peer closed".into(),
                    },
                ))
            },
        )))
        .expect("queued");

    assert!(ticket.wait().is_err());
    assert!(
        group.is_lost(),
        "an unusable transport must still retire the share group"
    );
    assert!(
        group.take_lost_report(),
        "the loss owes the application one GL_CONTEXT_LOST"
    );
}

/// The classification is on the failure's KIND, not on where it was caught: every kind that means the
/// connection is unusable retires the group, and every kind that means the host answered does not.
#[test]
fn only_an_unusable_transport_retires_the_group() {
    use hl_gpu::{GpuError, TransportError, TransportPhase};

    let phase = TransportPhase::Acknowledgement;
    for error in [
        GpuError::Transport(TransportError::Unavailable {
            phase,
            detail: "socket gone".into(),
        }),
        GpuError::Transport(TransportError::Timeout {
            phase,
            ambiguous: true,
        }),
        GpuError::Transport(TransportError::Ambiguous {
            phase,
            detail: "protocol desync".into(),
        }),
        GpuError::Transport(TransportError::ApiLost {
            detail: "capabilities changed".into(),
        }),
        GpuError::Transport(TransportError::Poisoned {
            cause: "earlier ambiguity".into(),
        }),
    ] {
        assert!(
            GlobalState::retires_share_group(&error),
            "an unusable transport retires the group: {error:?}"
        );
    }

    for error in [
        GpuError::Transport(TransportError::Rejected {
            phase,
            acknowledgement: 0,
        }),
        // The host answered — through a connection that is still there.
        GpuError::Invalid("malformed command"),
        GpuError::UnknownId {
            kind: "texture",
            id: 7,
        },
        GpuError::Panicked("backend defect the runtime rolled back".into()),
    ] {
        assert!(
            !GlobalState::retires_share_group(&error),
            "an answered request fails only itself: {error:?}"
        );
    }
}
