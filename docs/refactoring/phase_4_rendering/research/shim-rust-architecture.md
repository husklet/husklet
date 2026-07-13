# Guest rendering-shim architecture

Validated against current main on 2026-07-13. This document records stable ownership and capability
boundaries, not landed-work history or symbol-by-symbol status.

## Ownership

```text
guest application
  -> dd-shim-gl / dd-shim-vk / dd-shim-cuda / dd-shim-cudart
  -> dd-shim-common transport and negotiated capabilities
  -> dd-gpu shared IR + wire decoder
  -> software, wgpu/Metal, or CUDA executor
  -> dd-compositor surface composition
  -> dd-display presentation
```

- `dd-gpu` owns command types, encoding/decoding, identifiers, replay and the backend trait. A shim must
  not maintain a second handwritten wire protocol.
- `dd-shim-common` owns guest transport, memory registration, capability negotiation and connection
  recovery shared by API front ends.
- `dd-shim-gl` owns EGL/GLES object and error semantics and translates supported work to shared IR.
- `dd-shim-vk` owns the loader-facing ICD, Vulkan object model and Vulkan-to-IR translation.
- `dd-shim-cuda` and `dd-shim-cudart` own distinct public ABIs; shared allocation, transport and execution
  policy should remain below those ABI layers.
- `dd-compositor` owns Wayland protocol/resource/composition behavior. `dd-display` owns native window,
  input and presentation timing. Neither API shim may fabricate presenter completion.

## Supported contract

- EGL 1.4 and GLES 2.0 are the default advertised GL profiles. Their mandatory entry points have real
  bodies. GLES 3.0 is opt-in and its mandatory command bodies exist, but backend-dependent semantics such
  as occlusion query results remain explicitly degraded and are tracked in the Phase 4 backlog.
- The Vulkan ICD advertises core 1.4. Generated conformance inventory tests require real bodies for all
  mandatory core commands through that version. Unadvertised extensions may resolve only to truthful
  failure behavior.
- CUDA accepts only the modeled PTX/toolchain subset. Unsupported PTX and commands fail before partial
  backend mutation.
- A capability is usable only when the front end, negotiated wire version, IR, selected executor,
  compositor and presenter all support it. Exported symbols alone are not capability evidence.

## ABI and packaging rules

The shim crates are Rust `cdylib`s exporting vendor C ABIs. C ABI fixtures are therefore legitimate tests;
orchestration and assertions remain in Rust. Registry manifests are committed so guest builds need neither
network access nor registry XML at build time. Generation must fail on unknown ABI types, duplicate exports
or an advertised mandatory command without a real body.

Packaging owns loader-visible sonames and metadata: EGL/GLES libraries and `libwayland-egl`, the Vulkan ICD
library and JSON manifest, and CUDA driver/runtime sonames. Changing a crate or product name does not permit
changing these third-party ABI names. Phase 3 rebranding treats them as compatibility contracts.

## Failure and completion rules

- Unsupported work returns the API-correct error and initializes required outputs. Strict/debug modes are
  diagnostics, not substitutes for ordinary truthful failure.
- Decode, validation and capability rejection occur before mutating backend state.
- Submission acceptance and GPU/presentation completion are different events. Fence, query, swap and
  presentation results must use the completion event appropriate to their API.
- Reconnect/device loss invalidates host residency and causes safe resource reconstruction or a typed loss;
  it must not silently reuse stale identifiers.
- Tests prove C ABI results, errors, wire bytes, backend state, pixels, resource lifetime or timing. Reading
  source text to find an implementation is not a behavioral test.

## Maintained evidence

- GL profile contract: [`shim-gl-completeness.md`](shim-gl-completeness.md)
- CUDA profile contract: [`shim-cuda-completeness.md`](shim-cuda-completeness.md)
- Remaining cross-stack work: [`../backlog.md`](../backlog.md)
- Current verification snapshot: [`../validation.md`](../validation.md)

Per-command inventories belong in generated build artifacts and tests. Completed milestones, old counts and
branch handoffs remain available in Git rather than being copied into this file.
