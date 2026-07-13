# EGL/GLES capability contract

Validated against current main on 2026-07-13. The generated registry manifest is the symbol inventory;
this file records the supported profiles and known semantic boundary. It intentionally does not duplicate
hundreds of generated symbol rows or promotion history.

## Advertised profiles

| Surface | Default claim | Current status |
|---|---|---|
| EGL | 1.4 | all mandatory commands have real bodies and typed context/surface/thread/error behavior |
| OpenGL ES | 2.0 | all mandatory commands have real bodies and shared-IR/software execution coverage |
| OpenGL ES 3.0 | opt-in | mandatory command bodies exist, including instanced and base-vertex drawing; some backend-dependent semantics remain degraded |
| OpenGL ES 3.1/3.2 | not advertised as supported core | symbols may resolve for loader compatibility, but resolution is not a capability claim |

The build-generated capability inventory and its Rust tests are authoritative for current counts. A symbol
may be classified as full, a spec-valid partial/default, or unsupported with API-correct failure. No
unsupported mandatory command may silently succeed.

## Implemented behavioral families

- EGL display/config/context/surface lifecycle, share groups, per-thread current state, release-thread and
  API-correct error consumption.
- GLES program/shader, buffer, texture, framebuffer, renderbuffer, vertex-array and draw state translated
  through the shared IR.
- Texture upload/storage/copy/blit/resolve and subresource views supported by both modeled executors.
- Instanced, first-instance and base-vertex draw transport/execution.
- ES3 sampler, query, transform-feedback and uniform-block client object lifecycles, including validation,
  deletion and observable state.
- Fence/query lifecycle tied to submission serials rather than fabricated immediate success.

## Deliberate residuals

- Query lifecycle is real, but occlusion and transform-feedback counts are currently a truthful zero because
  no counter allocation/resolve contract reaches every backend. This is Phase 4 objective R2.
- Accepted submission and completed execution are not yet a fully asynchronous versioned transport contract.
  Sync behavior must not be stronger than the live executor acknowledgement. This is R1.
- Extension or later-profile commands outside the advertised profile can resolve for ABI compatibility only
  when they fail truthfully or perform a spec-valid no-op/default. They must not influence version strings.
- Real application traces may reveal shader, format or query semantics that unit inventory cannot prove.
  Those become behavioral gaps under R7, not reasons to inflate the advertised profile.

## Required gates

1. Generated inventory covers every export exactly once and rejects advertised mandatory stubs.
2. Rust tests exercise state/error/lifetime behavior through the public API; focused C fixtures verify the
   deployed C ABI and loader-visible libraries.
3. Shared commands round-trip through the production encoder/decoder and mutate both applicable executors
   consistently. Malformed or unsupported streams leave state unchanged.
4. Pixel and application tests close rendering claims. Source-text searches and export counts do not.

See [`../backlog.md`](../backlog.md) for residual acceptance criteria and
[`shim-rust-architecture.md`](shim-rust-architecture.md) for ownership boundaries.
