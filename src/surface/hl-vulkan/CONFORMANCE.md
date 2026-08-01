# Where the Vulkan surface stands

Written 2026-08-01 at the end of a session that measured this driver against Khronos VK-GL-CTS for the
first time with the diagnostics switched on. It records what is measured, what is unmeasured, what is a
capability gap rather than a defect, and what to do first. Numbers without a tree hash beside them are
not evidence; see the first section for why.

## Read this before quoting any number

**Bind every measurement to the Vulkan driver-tree hash, never to the bundle name or path.** Run:

```sh
mac python3 /Users/x/dd/e2e/husklet/apps/artifact.py     # driver_trees.vulkan is the identity
```

Two separate incidents in one session make this non-negotiable. A set of findings was attributed to a
bundle whose name had changed underneath them and survived only because the *driver tree* was identical
across both. And three runs were published-ready against a host worker twelve hours older than the
bundle, because an installed artifact does not tell you which process is serving it:

```sh
mac sh -lc 'ps -o pid,lstart,command -ax | grep "[w]orker domain <workspace>"'
```

If the worker predates the bundle build time, **kill it and re-run** — the harness reuses an already-open
workspace. That run reported 857 failures where a clean one reported 448. The guest driver is re-staged
from the bundle every run; the host executor is not.

Compare **isolated against isolated, never isolated against batched.** Twelve apparent regressions in
`memory.pipeline_barrier.graphics` all passed when the group was re-run alone — cascade collateral from an
earlier loss in the batch, not regressions. Batched numbers understate this driver.

## Measured, against Vulkan driver tree `9982fbbb94878e0e02db2a3bbdd768211095eae785e5f05349799c3956e1d0fc`

Baseline for all "before" figures is tree `65cbdb6169065570d23c42d3ccc778f04b3c3ad3643a767b55e33d5bdbdb8699`.

| group | before | after | note |
|---|---|---|---|
| `memory.*` | 1750 pass / 838 fail | 2164 pass / 427 fail | 0 real regressions |
| `memory.mapping` | 1522 pass / 711 fail | 1916 pass / 316 fail | integer-format lowering |
| `api.device_init` | 1 fail | **0 fail** | instance API-version fix |
| `api.format_features` | 24 fail | **3 fail** | `VkFormatProperties3` fill |
| `compute.*` + `api.smoke` | 73 fail | 73 fail | see the caveat below |

**The compute caveat, which must travel with the number:** zero regressions across 60817 cases sounds
strong until you notice only **29 of them actually pass** — 60715 are `NotSupported`. It is a weak
instrument for anything about format advertisement.

## Unmeasured

- `api.image_clearing` (45636 cases) — the clear-subresource and pass-segmentation fixes are unverified
  against the suite. A run of `image_clearing.core.*` was started at the end of the session; check
  `/Users/x/dd/hl-work/vk-cts/v2-clear/`.
- `api.copy_and_blit` (113317 cases, 26 minutes) — the copy/blit/resolve subresource fix is unverified.
  This is the largest group and the one most likely to move.
- Everything landed after tree `9982fbbb…`: the refusal mapping, the classified acknowledgement, the
  multisample refusal, the recording-error latch, 1D/3D support, `sampleCounts`. **None of it is
  measured.** A rebuild and re-run is the first thing worth doing.
- The harness wedged once mid-run — 37 minutes with no `deqp-vk` process alive and no output. If a group
  stops producing output, check for a live test binary before waiting on it.

## Capability gaps, not defects

These are things the driver correctly declines to claim. Do not "fix" them by advertising.

- **Depth comparison sampling.** The three remaining `format_features` failures are depth formats missing
  `VK_FORMAT_FEATURE_2_SAMPLED_IMAGE_DEPTH_COMPARISON_BIT` (bit 33, expressible only in the 64-bit
  `VkFormatProperties3`). `create_sampler` does not accept, let alone thread, `compareEnable`/`compareOp`
  — it builds its `SamplerDesc` with `..Default::default()`, so every comparison sampler is silently
  downgraded to a plain one. Declining the bit is honest; a shadow map sampling raw depth is the defect
  underneath, and fixing that comes first.
  Note `d16_unorm` *passed* before and fails now. It passed on uninitialised stack: the driver wrote
  nothing into `VkFormatProperties3` and the suite read whatever its own stack held. The case flipped
  because the driver started telling the truth.
- **16- and 32-bit integer formats.** `R16G16B16A16_UINT` and `R32G32B32A32_UINT` have no neutral-wire
  encoding, so they are unlowerable rather than unadvertised. Adding them is an `hl-gpu` wire change.
- **1D/3D for compressed and depth formats.** Optional per specification and refused deliberately; the
  executor's D1 path forbids the mip chain a block format needs.
- **`VERTEX_BUFFER` for single-component and BGRA formats.** The wire cannot express them.
- **Three `...2` queries do not walk their pNext chains** — `MemoryProperties2`,
  `ImageSubresourceLayout2`, `QueueFamilyProperties2`. Safe *only by advertisement*: every structure
  chainable onto them belongs to an unadvertised extension. This stops holding the moment one is
  advertised and nothing in the code will notice.

## Open defects, in the order worth taking

1. **The remaining 316 `memory.mapping` failures.** They changed from `INITIALIZATION_FAILED` to
   `DEVICE_LOST`: the format now lowers and the *host* refuses the frame. With the classified
   acknowledgement they should now report a real reason — read `<label>.driver.log` from the run rather
   than guessing.
2. **Tier 2 of the acknowledgement: the failing command's index.** Scoped but not small. Needs a typed
   batch failure at the `GpuExecutor::execute` port boundary in both the wgpu and CPU executors (the
   error carries no index today), plus a guest→host hello frame — this protocol has no channel for the
   guest to announce itself, so model it on the `READBACK_MAGIC` sentinel, whose version is deliberately
   independent of `WIRE_VERSION`. Carry the index **in the returned error value**, not in session state:
   a value moved out of the call is immune to the transaction rollback that would restore the state.
3. **`cmd_copy_buffer` accepts a self-copy of `u64::MAX` bytes on a 256-byte buffer and returns `Ok`,**
   where `cmd_copy_image` refuses an overlapping self-copy. Sibling disagreement, unproven, worth an hour.
4. **Untouched surface area.** Pipeline creation and state, descriptor set updates, render pass and
   framebuffer construction, and synchronisation primitives have had no sweep. Three questions worth
   carrying in: does every path that reads a count also read its array consistently; what happens when
   the guest omits state that is optional in the API but required by the host; and do two objects created
   from one description agree.

## Methods that actually found things

- **Sibling comparison.** Two of three creation paths guarding both bounds is not a policy. This found
  the buffer-size guards, the copy/blit/resolve subresources, and the refusal mapping that `hl-gl` had
  drawn and `hl-vulkan` had not.
- **Base rates before enrichment.** `image_clearing` showed 100% of device losses naming a layer — and
  100% of its case *names* contain "layer", so the signal was zero. `copy_and_blit` showed 76% against a
  9% base rate, and that one was real.
- **Poison every output before the call that should fill it.** Neighbouring tests passed because they
  zeroed the structure first, so the answer was right by the caller's initialisation rather than by
  anything the driver did.
- **Revert each half separately and check it actually flips.** Two tests in this session passed against
  deliberately broken code — one guarded by `if result == VK_SUCCESS` on a branch that never ran, one
  using a second command that returned `Ok`. Positive controls were solid; the discriminating controls
  were where the sloppiness showed.
- **Read the stream the subject writes.** `lsof -p <pid> | grep log`. The device-loss reason was in the
  guest driver log nobody collected, and the multisample validation error was in the host worker's
  `~/.hl/workspaces/<ws>/runtime/domain.log`.

## Running the suite

```sh
mac sh -lc 'cd /Users/x/dd/e2e/husklet/apps && python3 vk-cts/run.py \
    --group "dEQP-VK.memory.*" --work /Users/x/dd/hl-work/vk-cts/<name>'
```

`HL_VK_LOG` is set by the harness, which is what makes `<label>.driver.log` non-empty. It is off by
default in the driver and that default is correct — an ICD in a browser's GPU process must not write to
stderr unbidden — so fix the harness, never the default.
