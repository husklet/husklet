# Husklet — handoff, 2026-08-01

Read this first, then the per-area documents it points at. Written for someone
picking the work up cold.

---

## 1. What this project is

Husklet runs Linux containers on macOS arm64 **without a VM**. Guest
GL / Vulkan / CUDA calls are translated by shim libraries into a neutral IR
(`hl-gpu`), shipped over a transport to a host service, and replayed with wgpu
onto Metal. Output is presented zero-copy via memfd + IOSurface → CALayer.

Layers, guest to screen:

```
guest app → shim (hl-gl / hl-vulkan / hl-cuda) → IR (hl-gpu)
  → transport → host replay (hl-gpu-wgpu) → Metal
  → compositor (hl-compositor) → IOSurface → CALayer → window server
```

**Chrome is the final verification target.** A defect can live in any of eight
layers and present identically as "Chrome shows nothing" — that happened four
separate times on 2026-08-01.

Repos:
- `/Users/x/dd/husklet` — the product. Has a GitHub remote; push normally.
- `/Users/x/dd/e2e` — test harnesses. **Local only, no remote.** Commit locally.

---

## 2. Where Chrome actually stands

**Working now:**
- Native window opens, presents frames, survives.
- `chrome://gpu` reports all of 2d_canvas, gpu_compositing, rasterization,
  opengl, webgl, webgpu, video_decode accelerated.
  Renderer: `ANGLE (hl-gl, hl-gl-metal, OpenGL ES 3.1 hl-gl)`.
- Text rasterises correctly; widgets (scrollbar, drop shadow) composite.
- GPU transport healthy: 59 MB batches acked, `joined=5 refused=0 outstanding=0`.
- WebGL clear + `readPixels` is pixel-exact.

**Broken now:**
1. **Large black regions.** Page content and dialog surfaces composite black.
2. **Segfaults.** A user running Chrome interactively saw nine consecutive
   `Segmentation fault` lines. Never reproduced by any harness. Note they ran
   **Google Chrome** while harnesses run **Chromium** — uncontrolled variable.
3. **`L_OE` upload defect.** A texture uploaded from client memory reads back as
   ANGLE's own extension string. Survived nine rounds as a *distinct* defect;
   do not merge it into (1) without evidence.

### What (1) actually is — the most important section here

A texture that Chrome **renders into and then samples** resolves to nothing and
composites black. There are **at least three distinct causes**, which is why a
single fix will leave the symptom alive:

| GL name | state | cause |
|---|---|---|
| 107, 138 | `fbo_target_generations=[]` | never minted under any generation |
| 106 | holds `[335]`, sampler asks `364` | minted under a **stale generation** |
| 108 | `fbo_order=[64, 63]`, `render_target_of=[63]` | **sampled before the render**, rescued only by a previous frame's history |

Two bridges exist and neither covers the gap: `fbo_tex_ir` handles same-frame
render-then-sample, `resident_fbo_target_tex` handles cross-frame, and **neither
covers the first frame a given `(gl_tex, generation)` is sampled.** A texture
whose generation churns re-enters that state repeatedly.

Registration itself is **not** gated — 17 targets were minted across 11 GL names
in one run, measured.

**Fail-first signal, recorded precisely:** GL names 107, 138 and 106 must
resolve, while 108 still does.

Relevant code:
- `src/surface/hl-gl/src/service/frame/texture.rs:174` — the `stage_ir: 0` bind
  that is correct and not selected.
- `src/surface/hl-gl/src/service/frame/lower.rs:572` — the cross-pass guard that
  already exists for this case.
- `gpu_authoritative` is set only by `mark_rendered`, whose single caller runs at
  **swap** — so it can never be true for same-frame render-then-sample, which is
  exactly what a browser compositing its own canvas does.
- `build_multi` populates `fbo_tex_ir` from `run.first().and_then(|d| d.target)`
  while `resolve_target` falls back to the attachment table — a real divergence,
  separate from the three causes above, worth fixing on its own.

**A minimal client reproducing the presumed shape PASSES.** `gl-chrome-patterns`
builds an FBO colour attachment, samples it in the same frame, across real
`eglSwapBuffers` boundaries, on generations minted in the sampling frame —
nothing samples black. So the minimal shape is exonerated and **whatever Chrome
does in addition is the trigger**. Narrowing that is the next move.

Diagnostic and two *indicted* changes preserved unmerged at
`/Users/x/dd/hl-work/tex-fix`, commit `32011b0b`, marked not-for-merge. Do not
resurrect them as they stand: `rendered_into` is computed from the whole
recorded draw list, so it is true for a render not yet lowered; the changes
declare GPU-authority before the write and suppress the upload, binding
*nothing* rather than something wrong.

---

## 3. What was fixed today, and why each mattered

| commit | defect |
|---|---|
| `05a755cbf` | `GL_BGRA_EXT` was not colour-renderable while the driver advertised the extension whose defining property is that spelling. Chrome's raster tiles were framebuffer-incomplete. |
| `58d5c7b9f` | **Why Chrome had no window.** `glVertexAttribIPointer` discarded the integer flag at the entry point, so attributes lowered to `Float32x2` while Chrome's shader required `Uint32x2`. wgpu refused the pipeline; the refusal rolled back Chrome's entire 1121-command startup batch, and every later frame referenced resources that had evaporated. Broken since 2026-07-23; no test drove the path. |
| `66343a8ee` | **Why no Vulkan client ever presented.** `create_swapchain` omitted `texture_usage::PRESENT`, so IOSurface backing was refused and the producer skipped the frame down a silent `continue` while the guest saw `VK_SUCCESS`. The comment beside the omission already named the correct set. |
| `bf864564f` | `has_content()` answers "may this be shown"; three call sites were asking whether content had been *committed*. A GPU token is a promise to present, so a toplevel counted as mapped before its first commit and the not-mapped-to-mapped transition never fired. |
| `dcaea6e2e`, `7e7dbbf62` | Four Vulkan shim sites dropped a recorded refusal (`let _ =` where every sibling latched); swapchain usage derived from one source; `vkCmdFillBuffer` honours `VK_WHOLE_SIZE` rounding; vertex layouts indexed by binding number. |
| `d471e287a` | 636 block-compressed blit cases advertised then refused at record time — un-advertised honestly, verified with a null control *and* a positive control. |

128 commits total on 2026-08-01.

---

## 4. Conformance — the numbers, on stated bases

**Never quote the two bases interchangeably.** They describe the same run and
only one is a coverage claim.

| suite | executed | enumerated | cases |
|---|---|---|---|
| dEQP-GLES2 | **54.6%** (9543/17485) | — | 17,485 |
| dEQP-GLES3 | **46.7%** (21246/45476) | — | 45,476 |
| dEQP-VK api+memory | **96.2%** (10019/10417) | 98.5% | 26,094 |
| dEQP-VK copy_and_blit.core | **27.7%** (1131/4082) | 97.8% | 134,125 |

"Executed" excludes `NotSupported`; "enumerated" counts declines as
not-passing. Warnings count as passes — dEQP's own convention.

GLES2 and GLES3 are **not comparable** (different modules, different bundles).

**The cascade** — one wedged context poisoning every later case in the same
process — is measured three ways and differs sharply by API:

| suite | cascade cost |
|---|---|
| GLES2 | ≈17.8 points (36.8% batched → 54.6% isolated, bounded not exact) |
| GLES3 | **11.0 points** (35.7% → 46.7%, measured exactly) |
| Vulkan | ~0.02 points (2 of 400 cases) |

**Consequence: batching is safe for Vulkan conformance and unsafe for GLES.
The rungs must not share a runner policy.** Always snapshot `results-batched.json`
the moment the sweep returns, before isolation mutates verdicts in place — GLES2
failed to and can only report a range.

Whole Vulkan CTS is **3,252,976** cases; the scored subset is 0.8% of it.
`dEQP-VK.wsi` (36,880 cases, presentation) has **never been run** and is the
module closest to this project's actual goal.

---

## 5. How to run the suites

All Husklet sessions run on the **macOS host**, not in the Linux VM. Use the
`mac` command, and expand `~`/`$HOME` **on the far side**:

```sh
mac sh -c 'ls $HOME/.hl'      # correct
mac ls ~/.hl                  # WRONG — your local shell expands ~ first
```

### Build a bundle (never install over /Applications while others measure)

```sh
cd /Users/x/dd/husklet
PATH=/Users/x/dd/hl-work/gitshim:$PATH make app     # → target/Husklet.app
# make install copies to /Applications — only when you mean it
```

`make app` targets `$(CURDIR)/target/Husklet.app`. If nix fails with
`error: tool 'git' not found`, the gitshim on PATH fixes it (Xcode stub leaking
into the nix shell).

### GLES2 / GLES3 conformance

```sh
cd /Users/x/dd/e2e/husklet
mac python3 apps/deqp/run.py --module gles3          # sweep
mac python3 apps/deqp/run.py --resume                # continue an interrupted sweep
```
Baselines: `apps/deqp/BASELINE.md` (GLES2), `apps/deqp/BASELINE-GLES3.md`.

### Vulkan conformance

```sh
mac python3 apps/vk-cts/run.py --group 'dEQP-VK.api.<subgroup>.*'
mac python3 apps/vk-cts/isolate.py --from <results.json> --case-timeout 30 --chunk 50
./apps/vk-cts/score.py <results.json> --isolated <isolated.json>
```
Run each subgroup as its own group: a group is one process lineage, so a
subgroup that wedges cannot cost the ones after it.
Baseline: `apps/vk-cts/BASELINE.md`.

### Chrome / browsers

```sh
mac python3 apps/browsers/run.py chrome              # headless, all cases: omit the arg
mac python3 apps/browsers/windowed.py --skip-control # windowed, TAKES HOST FOCUS
mac python3 apps/browsers/windowed.py --case chromium
```
`HL_APP_BUNDLE=/path/to/Husklet.app` selects a bundle other than `/Applications`.
Cases: `chromium`, `chromium-sandboxed`, `chrome`, `chrome-nosandbox`,
`chrome-software` (the `--disable-gpu` control — keep it in every comparison),
`firefox`.

### Fast GL pattern clients (seconds, not minutes)

```sh
mac python3 apps/gl-chrome-patterns/run.py
```
Five patterns: `render_sample`, `int_attrib`, `tex_upload`, `bgra_tile`,
cross-frame. See `apps/gl-chrome-patterns/RESULTS.md`.

### Differential vs Mesa llvmpipe

```sh
mac python3 apps/gl-diff/run.py --self-test
mac python3 apps/gl-diff/run.py --case corpus:render_to_texture --keep-images
```

### Windowed Vulkan

```sh
mac python3 apps/vk-windowed/run.py
```

### Diagnostics — read this or you will measure nothing

`hl-log` masks by **tag**, and the mask defaults to **empty**. An unenabled
diagnostic makes the *subject* look silent.

```sh
HL_LOG=present,gl,transport,gpu HL_LOG_LEVEL=debug
```

- `HL_LOG` is read **once, when the execution domain starts**. A reused domain
  keeps its old mask and setting the variable changes nothing.
- warn/info/debug are **compiled out** in release. Only error survives.
- `verdict=ack` is `Level::Debug` (compiled out) while `verdict=nack` is
  `hl_error!` (survives) — so `grep -c ack` returning zero on a release log is
  **normal**, not a dead instrument.
- Host-side diagnostics land in `~/.hl/workspaces/<name>/runtime/domain.log`,
  not the session log.
- Structured events: `hl_event!` / `hl_verdict!`, one JSON record per line with
  call-site provenance. Consumer guide in `apps/RUNBOOK.md`.

---

## 6. What to do next — priority order

### 1. Submit-loop refusal propagation — **highest value, fully specified, unbuilt**

`src/gpu/hl-gpu-wgpu/SUBMIT_PROPAGATION.md`

54 `?` operators inside `submit_cb_inner`'s loop each abort the **entire**
command buffer, and the error returns **before** `submit_encoded`, so
already-encoded native work is silently dropped. This is the amplification that
turned one refused pipeline into a dead browser. Pre-existing, general, and it
converts any single defect into total failure — which is why localising took
hours all day.

The document has the five-step fix including the load-bearing detail: precompute
the loop advance before running the op, so a refusal cannot leave `i` unmoved or
land inside a pass body.

### 2. Chrome's black content

Section 2 above, plus `e2e/husklet/apps/browsers/README.md`. Three causes; the
minimal shape is exonerated; find what Chrome does in addition.

### 3. Chrome segfaults

Unreproduced. Re-run on a workspace whose image already carries the browser
(`chrome-arm-probe`) so the time budget goes to the browser rather than an
800 MB unpack, and control the Chrome-versus-Chromium variable.

### 4. Vulkan 3D blits — layer 3

`src/surface/hl-vulkan/BLIT_HANDOFF.md`. Layers 1 (oracle) and 2 (recorder)
landed; layer 3 is `hl-gpu-wgpu/src/blit.rs`, one draw per destination slice.
**There is a deliberate divergence right now**: the CPU oracle serves `D3` blits
and the wgpu executor still refuses them. That is intended and is what makes the
executor layer measurable — it is stated at both refusal sites.

352 cases. No query lets a driver decline 3D blits and none lets an application
discover it, so this is an unannounceable hole in core Vulkan 1.0: it must be
served, not declined.

### 5. CUDA graphics interop

`src/gpu/hl-gpu/HANDOFF.md`. **0 of 23 interop entry points exist** — not
untested, absent — so no CUDA renderer or CUDA video path can present at all.
Export registry and access gate landed; protocol commands and executor wiring
remain. Note the gate is currently **unreachable in a shipped bundle**
(`set_guard` has zero production callers, `Exports` is never constructed).

### 6. Coverage gaps worth closing

- `dEQP-VK.wsi` — 36,880 presentation cases, never run, closest to the goal.
- Vulkan integer blits — 100 cases. wgpu has no native blit; it samples through
  a full-viewport draw with a hardcoded `texture_2d<f32>`, so integer textures
  cannot bind. Needs WGSL variants with integer outputs and sampler-less layouts.
- `image_clearing` (45,636), non-`core` copy variants (67,221),
  `object_management` (457 — never produced a verdict, cause unestablished).
- Third-party Vulkan apps: vkmark and the Sascha Willems samples are now
  reachable (provisioning made version-agnostic) and nobody has run them. Two
  clients prove a path; a real app with textures, depth and multiple pipelines
  would prove the capability.

---

## 7. Traps — each of these cost real hours today

**Measurement**
- Bind every run: binary hash, **guest driver subtree hash read from inside the
  guest**, and confirm the domain started *after* any install. Three conclusions
  were void for missing this. A same-path rebuild does **not** rebind a running
  domain, and no artifact header reports that.
- A zero is only a zero if the channel is proven. Show the instrument spoke
  before reading silence as a finding.
- Never truncate a stream something downstream parses. `grep verdict=nack`
  returns only the first line of a multi-line wgpu error — the answer sat four
  lines below and cost a day.
- Grep patterns collide with subject vocabulary: `grep -i segmentation` matched
  Chrome's `segmentation_platform` component and reported twelve crashes that
  were not crashes.
- A ratchet reports regression, not correctness. `ci/glmark2-baseline.json`
  records scenes as `pixels-wrong`, so its "green" means "no worse than the
  recorded broken floor". `ci/rung.py` is the shape to copy.

**Build**
- `cargo check --workspace --all-targets` is **not** the whole tree. Five crates
  declare their own `[workspace]` and sit outside the root members list:
  `hl-vulkan/shim/vulkan`, `hl-gl/shim/egl`, `hl-cuda/shim/{cuda,cudart,nvml}`.
  All five depend on `hl-gpu`. Check each explicitly when touching a shared type.
- `hl-gpu-wgpu` tests need Metal — run on the host:
  `mac cargo test -p hl-gpu-wgpu --test blit_mirror` (~12s incremental).
- The guest cross-linker `aarch64-linux-gnu-gcc` is **absent** in the Linux VM.
  Anything triggering a guest cross-build must run on the host.

**Environment**
- The git index is shared. Always `git commit --only -- <paths>`; a bare commit
  sweeps up other people's staged files.
- `~/.hl/workspaces.conf` sections are `[workspace]` + `name = x`, not
  `[x]`. A malformed section breaks **every** workspace on the host.
- Execution domains outlive their sessions by design, so a live domain is not
  evidence of a live run — and an orphan blocks the next one while looking like
  a product fault.

**Code**
- A comment asserting a capability is a hypothesis. Three load-bearing false
  comments were found today; one sat on the line that broke every Vulkan window,
  and one described an exclusion the code could not perform.

---

## 8. Documents

| path | covers |
|---|---|
| `SESSION-2026-08-01.md` | session index |
| `AGENTS.md` | measurement discipline, each rule paid for by a specific mistake |
| `src/gpu/hl-gpu-wgpu/SUBMIT_PROPAGATION.md` | the whole-buffer abort, specified |
| `src/surface/hl-vulkan/BLIT_HANDOFF.md` | 3D + integer blits |
| `src/gpu/hl-gpu/HANDOFF.md` | CUDA interop slices |
| `src/gpu/hl-gpu/SHARING.md` | cross-connection buffer sharing design |
| `src/surface/hl-cuda/INTEROP.md` | the 23 missing interop entry points, ranked |
| `e2e/husklet/apps/RUNBOOK.md` | harness conventions, structured-event consumer guide |
| `e2e/husklet/apps/browsers/README.md` | Chrome findings and traps |
| `e2e/husklet/apps/gl-diff/README.md` | differential harness and its two blind spots |
| `e2e/husklet/apps/gl-chrome-patterns/RESULTS.md` | fast pattern clients |
| `e2e/husklet/apps/deqp/BASELINE*.md` | GLES2 and GLES3 baselines |
| `e2e/husklet/apps/vk-cts/BASELINE.md` | Vulkan baseline |
| `e2e/husklet/apps/vk-windowed/README.md` | windowed Vulkan result |
| `e2e/husklet/apps/guest-fs/README.md` | guest toolchain, written as a retraction |

`docs/` is **gitignored** — do not put tracked documents there.

---

## 9. Honest limits of this handoff

- No failure in any conformance run has an explanation attached. Scored runs
  emit no guest-driver diagnostics; every named cause here came from a
  `domain.log` that happened to have the right tag open.
- The frame-time drift (~1.27 late/early over two minutes) is unresolved. Both
  registered hypotheses were falsified by their own controls.
- The Vulkan format table is wrong in **both** directions — 1,172 advertised and
  refused, 30 required and missing. Editing one direction alone makes the other
  worse; the warning is on `features()` itself.
- Roughly half of 2026-08-01 went to instruments rather than driver code. If
  something looks impossible, suspect the instrument first — it was the
  instrument's fault more often than not.
