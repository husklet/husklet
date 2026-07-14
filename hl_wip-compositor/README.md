# hl_wip-compositor — the platform-neutral compositor policy (scene)

**Status: the `scene` brain is IMPLEMENTED and unit-tested.** This is a standalone crate (package
`hl_wip_compositor`, lib `hl_compositor`, empty `[workspace]` — excluded from the shared repo
workspace) implementing OVERVIEW-v2 §7's *platform-neutral* compositor policy: the window tree, damage,
popup placement, frame pacing/scheduling, focus, and surface-commit rules. Zero dependencies — pure
std — so it builds and tests with **no Smithay, no GPU, no Cocoa/DRM**.

```
cargo test --manifest-path hl_wip-compositor/Cargo.toml
```

## What this crate IS

The compositing "brain": everything a Wayland compositor *decides* that does not depend on the wire
protocol, the GPU, or the host windowing system. It is organized by the four uniform roles (§2):

```
src/
  lib.rs                         crate facade (re-exports scene::Compositor + outcomes)
  scene/
    mod.rs                       the Compositor wiring object (scene + Presenter + Clock)
    model/                       the neutral values + invariants
      scene.rs                   the aggregate scene graph + tree navigation
      surface.rs                 Surface, buffer/viewport, Format, Visibility, PresentableImage
      window.rs                  roles (toplevel/popup/subsurface/cursor), Positioner, PopupPlacement
      output.rs                  Output: mode, refresh interval, scale, logical size
      seat.rs                    keyboard/pointer focus state
      damage.rs                  Rect + DamageRegion
    port/                        the two boundary traits (inward contracts)
      presenter.rs               Presenter (present PresentableImage) + PresentOutcome/Timing/Feedback
      clock.rs                   Clock (now_nanos)
    service/                     the use-cases (one operation per file)
      commit.rs                  apply a surface commit to the scene + mark damage
      popup.rs                   xdg_positioner → on-screen placement (flip/slide/resize)
      compose.rs                 walk the tree → ordered present layers + damage; occlusion skip
      schedule.rs                frame pacing state machine + vsync throttle
      focus.rs                   keyboard/pointer focus + window activation + hit-testing
tests/scene.rs                   16 fake-driven tests (FakeClock + FakePresenter)
```

The whole policy is expressed against the two ports, so it is fully testable with a fake clock (scripted
nanos) and a fake presenter (records `present()` calls).

## What is DEFERRED (later tasks — NOT built here)

Per §7 these are adapter concerns and are deliberately absent:

- **`adapter/smithay`** — Wayland protocol dispatch translating `wl_*`/`xdg_*` callbacks into the
  `scene::service` calls. The Smithay-required `HlState` aggregate lives here too.
- **`adapter/cocoa`** (macOS) and **`adapter/drm`** (Linux) — concrete `scene::port::Presenter`
  implementations (Cocoa/Metal/IOSurface; DRM/GBM/KMS scanout).

## Provenance

The algorithms are ported (platform-neutral extraction) from `hl-compositor`'s `lib.rs`
(`HlState` present bookkeeping / focus), `handlers/compositor.rs` (commit→present, damage, compose
walk, occlusion, pacing state machine) and `handlers/xdg.rs` (window/popup tree, positioner), plus the
`Presenter`/`SurfaceBuffer`/`PresentOutcome`/`PresentTiming`/`PopupPlacement` value shapes from
`hl-display/src/present.rs`. The popup placement math re-implements `xdg_positioner` (flip → slide →
resize) that the ported code delegated to Smithay's `PositionerState`.
