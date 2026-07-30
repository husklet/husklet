use super::*;

/// Executor-NACK atomicity (the Chrome swap-NACK recovery). A frame creates a fresh tile texture and
/// destroys the old one, then the swap's present NACKs (device validation) — the executor has ALREADY
/// applied the create + destroy by the time it fails. The whole submit must be atomic: the id tables AND
/// the residency ledger are rolled back to EXACTLY the pre-frame state, so the connection recovers —
/// destroying the (restored) old id does not `UnknownId`, and recreating the (rolled-back) new id does
/// not `DuplicateId`. Before the fix this drifted: the old texture was gone (its later destroy hit
/// `UnknownId`) and the new one stuck live + charged (its recreate hit `DuplicateId`), NACKing forever.
#[test]
fn executor_nack_rolls_back_tables_and_ledger_then_connection_recovers() {
    let caps = Capabilities::full("fake");
    let mut exec = NackOnPresentExecutor::new(caps.clone());
    // Unbounded residency so the ONLY rejection here is the executor's dispatch-stage NACK, not accounting.
    let mut s = session(
        Limits::from_capabilities(caps.clone()),
        GlobalLedger::unbounded(),
    );

    // A prior frame created the "old" tile (id 1); it is committed and live on the executor.
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(1, 8)]).expect("old tile created");
    let tile_bytes = 8 * 8 * 4; // one 8x8 rgba8 mip
    assert_eq!(
        (
            s.residency_bytes(),
            s.object_count(),
            s.resources.live_count()
        ),
        (tile_bytes, 1, 1)
    );

    // The swap frame: create the new tile (id 2), free the old tile (id 1), then present -> NACK. The
    // executor applies the create + destroy, then fails on the present.
    let surf = SurfaceDesc {
        width: 8,
        height: 8,
        format: TextureFormat::Rgba8Unorm,
        token: hl_gpu::SurfaceToken::new(1).unwrap(),
    };
    let swap = vec![
        texture(2, 8),
        Cmd::DestroyTexture(1),
        Cmd::CreateSurface(30, surf),
        Cmd::Present {
            surface: 30,
            texture: 2,
            serial: hl_gpu::FrameSerial::new(1).unwrap(),
        },
    ];
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 256, &swap).unwrap_err();
    assert_eq!(
        err,
        GpuError::Invalid("wgpu: pass failed device validation")
    );

    // Atomicity: the NACKed frame left the tables + ledger EXACTLY as before it — old tile (1) restored,
    // new tile (2) and the surface (30) rolled back, residency back to just the old tile.
    assert_eq!(
        (
            s.residency_bytes(),
            s.object_count(),
            s.resources.live_count()
        ),
        (tile_bytes, 1, 1),
        "a NACKed frame charges no residency and leaves exactly the pre-frame objects"
    );
    assert!(
        s.resources.textures.contains(1),
        "the freed old tile is restored (destroy was rolled back)"
    );
    assert!(
        !s.resources.textures.contains(2),
        "the created new tile is gone (create was rolled back)"
    );
    assert!(
        !s.resources.surfaces.contains(30),
        "the created surface is gone (create was rolled back)"
    );

    // Recovery 1 — destroying the (restored) old tile now SUCCEEDS (pre-fix this was `UnknownId`).
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[Cmd::DestroyTexture(1)])
        .expect("old tile is destroyable after the NACK rolled its destroy back");
    assert_eq!((s.residency_bytes(), s.object_count()), (0, 0));

    // Recovery 2 — recreating the new tile now SUCCEEDS (pre-fix this was `DuplicateId`), and the whole
    // connection keeps working: the retried swap present NACKs cleanly and is again fully rolled back.
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(2, 8)])
        .expect("new tile id is free again after the NACK rolled its create back");
    assert_eq!(
        (
            s.residency_bytes(),
            s.object_count(),
            s.resources.live_count()
        ),
        (tile_bytes, 1, 1)
    );
}

/// The residency-cap create/free/recreate loop (Chrome's bounded live tile set). With old tiles freed each
/// frame the live set stays UNDER the per-connection cap; a frame that would exceed the cap is NACKed
/// atomically (`ResourceLimit("connection residency")`) and — crucially — the connection RECOVERS: freeing
/// old tiles and creating new ones in the next frame keeps working with no `UnknownId`. This is why the
/// 512 MiB / 65 536-object cap need not be raised: a well-behaved client with a bounded working set that
/// retires what it no longer needs never sticks against it, while the cap still bounds a hostile flood.
#[test]
fn residency_cap_nacks_over_budget_frame_then_free_and_recreate_recovers() {
    let caps = Capabilities::full("fake");
    // Mirrors texture lifecycle into the tables; NACKs only on `Present` (this test issues none).
    let mut exec = NackOnPresentExecutor::new(caps.clone());
    let tile_bytes = 8 * 8 * 4u64; // one 8x8 rgba8 tile
                                   // A cap sized for EXACTLY two live tiles — a tiny stand-in for the real bounded working set.
    let mut limits = Limits::from_capabilities(caps);
    limits.max_connection_bytes = 2 * tile_bytes;
    limits.max_connection_objects = 2;
    let mut s = session(limits, GlobalLedger::unbounded());

    // Fill the working set to the cap: two live tiles.
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(1, 8)]).expect("tile 1");
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(2, 8)]).expect("tile 2 (at cap)");
    assert_eq!((s.residency_bytes(), s.object_count()), (2 * tile_bytes, 2));

    // A swap frame that creates one more tile without freeing an old one trips the cap and NACKs. It is
    // rejected atomically at ACCOUNT: nothing charged, nothing reached the executor.
    let live_before = s.resources.live_count();
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(3, 8)]).unwrap_err();
    assert_eq!(err, GpuError::ResourceLimit("connection residency"));
    assert_eq!(
        (s.residency_bytes(), s.object_count()),
        (2 * tile_bytes, 2),
        "NACK charged nothing"
    );
    assert_eq!(
        s.resources.live_count(),
        live_before,
        "NACKed frame never reached the executor"
    );

    // Recovery — the Chrome create-tile / free-old-tile / swap step: free the two old tiles and create two
    // fresh ones in one frame. Net residency stays at the cap, so it fits, and every destroy resolves a
    // LIVE id (no `UnknownId`). The connection keeps working.
    let loop_frame = vec![
        Cmd::DestroyTexture(1),
        Cmd::DestroyTexture(2),
        texture(3, 8),
        texture(4, 8),
    ];
    hl_gpu::runtime::submit(&mut s, &mut exec, 256, &loop_frame)
        .expect("free-old + create-new fits the cap");
    assert_eq!((s.residency_bytes(), s.object_count()), (2 * tile_bytes, 2));
    assert!(s.resources.textures.contains(3) && s.resources.textures.contains(4));
    assert!(!s.resources.textures.contains(1) && !s.resources.textures.contains(2));
}

/// A backwards timeline-fence signal is a typed rejection the runtime raises AFTER the executor has
/// already accepted the batch (`dispatch` reflects the fence lifecycle only once `execute` succeeded).
/// That rejection must still be atomic: the frame's creates must not survive it, the ledger must not
/// drift, and the timeline must not move — otherwise a guest that signals backwards every frame leaves
/// resources live on the executor that the ledger no longer accounts for, defeating the residency bound.
#[test]
fn a_backwards_fence_signal_rolls_back_the_whole_frame() {
    let caps = Capabilities::full("fake");
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(
        Limits::from_capabilities(caps.clone()),
        GlobalLedger::unbounded(),
    );

    let signal = |value| {
        Cmd::Submit(CommandBuffer {
            encoder: Vec::new(),
            signal: Some((1, value)),
        })
    };
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[Cmd::CreateFence(1), signal(10)])
        .expect("fence created and signalled to 10");
    let before = (
        s.residency_bytes(),
        s.object_count(),
        s.resources.live_count(),
    );

    // This frame creates a buffer and then signals fence 1 BACKWARDS (5 < 10).
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[buffer(7, 4096), signal(5)])
        .unwrap_err();
    assert_eq!(
        err,
        GpuError::Invalid("fence timeline value moved backwards")
    );
    assert_eq!(
        (
            s.residency_bytes(),
            s.object_count(),
            s.resources.live_count()
        ),
        before,
        "a rejected frame leaves the tables and the ledger exactly as they were"
    );
    assert_eq!(
        s.timeline.get(1),
        Some(10),
        "the timeline must not move on a rejected frame"
    );

    // The connection recovers: the rolled-back id is free to be created again.
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[buffer(7, 4096)])
        .expect("the rolled-back id is creatable again");
}
