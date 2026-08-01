# CUDA graphics interop — the map

Scoping only. Nothing here is implemented and nothing here proposes that it should be until the ordering
below is agreed.

## The measurement

Zero of twenty-one CUDA graphics-interop entry points exist. Measured on the source at HEAD `8e8e69600`,
not inferred: a search of `src/` for each of the names below returns nothing at all, and
`e2e/husklet/apps/cuda/probe.c::stage_interop` probes all twenty-one by `dlsym` and by the driver's own
`cuGetProcAddress` in both `libcuda.so.1` and `libcudart.so.1`, asserts `present > 0`, and is honestly
red.

This is a product-shaped hole rather than a test gap. It sits directly on the goal that any GUI
application presents through the native accelerated surface: **every CUDA application that renders — and
every CUDA-accelerated video path — cannot present at all.** `e2e/husklet/apps/cuda-present` demonstrates
the round-trip-through-host-memory alternative works end to end, but that is the route an application
must have been *written* to take. An application built on `cuGraphicsGLRegisterBuffer` fails at the first
call.

## The one thing that blocks all of it

**`hl-gpu` resources are per-connection.** `runtime::model::resources::SessionResources` maps protocol
ids to executor-native objects, one table per connection. `GlobalLedger` is shared, but it is residency
*accounting* only — it holds no resources and confers no addressability.

The guest CUDA driver and the guest GL driver are **separate connections** to the same host executor over
`$HL_GPU_EXEC`. So a `BufferId` minted by the GL session is meaningless in the CUDA session. Every tier-1
entry point below is, underneath, one request: *let this session address a resource another session
owns.* Nothing in the protocol expresses that today.

That is the foundational piece and it is most of the work. It is also the piece worth designing once,
because Vulkan↔CUDA (tier 3) needs the identical capability.

**The design for it is `src/gpu/hl-gpu/SHARING.md`** — the mechanism belongs to `hl-gpu` because the
limitation does. It covers identity, lifetime, and every failure edge (owner frees under a live import, a
handle from a departed connection, and the two distinct races), and it states explicitly what it does not
make safe.

## What is cheap, and the reason it is cheaper than it looks

**Tier 1 needs no new memory-sharing mechanism whatsoever.** This is the load-bearing observation in this
document, and it inverts the obvious cost estimate.

A CUDA device buffer and a GL buffer object are *both already host-side resources in the same executor
process*. Neither is guest memory. So CUDA↔GL **buffer** interop never crosses the guest/host boundary,
never needs a memfd, never needs an IOSurface, and never needs a copy. It is an aliasing problem inside
one process — a cross-session handle in the `hl-gpu` protocol plus a lifetime rule — and the executor
already holds both objects.

Mechanisms that *do* already exist, and what they are actually good for:

| existing mechanism | where | what it can serve |
|---|---|---|
| IOSurface-backed textures | `hl-gpu-wgpu/src/iosurface.rs`, consumed by `present.rs` | tier 2 image interop, and any path that must reach the compositor |
| guest→host image import over a Husklet-private dmabuf modifier whose plane fd is a memfd | `hl-gl/shim/egl/src/driver/platform/image.rs` | tier 2/3, where guest-visible memory genuinely must be shared |
| the `hl-gpu` resource table and its uniform lifecycle checking | `runtime/model/resources.rs` | the natural home for an export/import handle; duplicate-create, use-after-free and double-free are already enforced there once, for every resource kind |

Vulkan external memory is **not** available to build on: `VK_KHR_external_memory` appears in
`hl-vulkan/src/model/capability.rs` only. Tier 3 is therefore two-sided work, not one-sided.

## The twenty-one, ordered by applications unblocked per unit of work

### Tier 1 — buffer interop. The whole dominant pattern, and the cheapest.

CUDA computes into a GL buffer (a PBO for pixels, a VBO for geometry) and GL draws it. This is what the
overwhelming majority of real CUDA↔GL applications do, and it is what a CUDA renderer, a CUDA particle
system, and most framework display paths reduce to.

| entry point | note |
|---|---|
| `cuGraphicsGLRegisterBuffer` | the export/import handle; the real work |
| `cuGraphicsMapResources` | map/unmap become ordering points against the CUDA stream, not copies |
| `cuGraphicsUnmapResources` | |
| `cuGraphicsResourceGetMappedPointer_v2` | returns a `CUdeviceptr` aliasing the GL buffer |
| `cuGraphicsUnregisterResource` | |
| `cudaGraphicsGLRegisterBuffer`, `cudaGraphicsMapResources`, `cudaGraphicsResourceGetMappedPointer` | thin runtime mirrors of the above; near-free once the driver side exists |
| `cuGLGetDevices_v2`, `cudaGLSetGLDevice` | one device; essentially constant answers, but applications call them first and fail early without them |
| `cuGraphicsResourceSetMapFlags_v2` | a read-only/write-only *hint*. Can be a validating no-op — but it must validate, not accept anything, or it is a capability claimed and not honoured |

**Ten of the twenty-one, and the largest share of real applications.** The nine after the first four are
small once the handle exists.

### Tier 2 — image/texture interop. Needed by fewer applications, and substantially more work.

| entry point | note |
|---|---|
| `cuGraphicsGLRegisterImage` | |
| `cuGraphicsSubResourceGetMappedArray` | returns a `CUarray` |

The blocker is not the sharing: it is that **hl-cuda has no CUDA array or surface object at all** — no
`cuArrayCreate`, no `cuSurfObjectCreate`, and `cuModuleGetSurfRef`/`cuModuleGetTexRef` exist without any
array behind them. Tier 2 therefore requires building a CUDA resource kind that does not exist, before
any interop question is reached. IOSurface makes the host half tractable; the driver half is the cost.

### Tier 3 — external memory and semaphores. Real, but two-sided.

| entry point | note |
|---|---|
| `cuImportExternalMemory`, `cuExternalMemoryGetMappedBuffer`, `cuDestroyExternalMemory` | CUDA↔Vulkan memory |
| `cuImportExternalSemaphore` | cross-API synchronisation |
| `cuSignalExternalSemaphoresAsync`, `cuWaitExternalSemaphoresAsync` | **not among the twenty-one, and required.** An imported semaphore is inert without them |

This is how modern engines do interop, and it is the direction the ecosystem has moved. But `hl-vulkan`
does not implement external memory either, so this needs both sides. It should reuse whatever cross-session
handle tier 1 establishes rather than inventing a second one.

**The count for a working tier 3 is twenty-three, not twenty-one.** `probe.c` probes twenty-one entry
points and neither signal nor wait is among them, so the harness's own number understates the work. That
matters in the direction that hurts: an undercount produces an underestimate at exactly the moment
someone is deciding whether the tier is affordable. The twenty-one figure is correct for "what is
missing that we currently measure" and wrong for "what building this costs".

### Tier 4 — do not build without a named consumer.

| entry point | note |
|---|---|
| `cuGLCtxCreate_v2` | deprecated by NVIDIA; superseded by `cuGraphics*` |
| `cuGraphicsEGLRegisterImage`, `cuEGLStreamConsumerConnect` | EGLStream, effectively Tegra-only; almost nothing on desktop Linux uses it |

## Two cautions carried from elsewhere in this codebase

**An extension string is a promise to every application, not only the one being traced.** Exporting a
`cuGraphics*` symbol is exactly such a promise. A registered resource that maps to a pointer nothing
honours is worse than an absent symbol, because the absent symbol fails at `dlsym` where an application
can fall back, while a lying one fails as silently wrong pixels.

**Enumerate every path that must serve a widened capability before shipping it.** A format ungate
elsewhere in this repository moved 783 cases from honestly declined to running and failing, because three
of four paths learned a new image type and the fourth did not. Tier 1 has the same shape: registering,
mapping, addressing and destroying must all learn the cross-session handle, and the lifetime rule must
answer what happens when the owning session disconnects while the borrowing one still holds a mapping.

## What a first slice should have to prove

Not proposed for build — recorded so the criteria exist when it is:

- a CUDA kernel writes a GL buffer registered through `cuGraphicsGLRegisterBuffer`, GL draws it, and the
  presented frame is compared against a reference computed outside both, exactly as
  `e2e/husklet/apps/cuda-present` already does for the copy route;
- the same case with the round trip removed measurably shows the copy is gone, rather than being assumed;
- a mapped resource used by CUDA *while still mapped by GL* is refused, with a positive control on the
  same path proving the refusal is validation and not a broken path;
- the owning session disconnecting under a live mapping does something defined and testable.
