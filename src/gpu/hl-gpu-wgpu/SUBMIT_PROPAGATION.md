# `submit_cb_inner`: per-operation refusal instead of whole-batch abort

Written 2026-08-01. **Implemented on 2026-08-02** by `161d26cfc` (in-pass refusal handling) and
`faaa28df0` (top-level continuation), with positive-control coverage in `tests/submit_refusal.rs`. The
original specification remains below as the rationale and regression checklist.

Subject: `src/gpu/hl-gpu-wgpu/src/submit.rs`, `WgpuExecutor::submit_cb_inner` (~840 lines, the
`while i < ops.len()` loop that begins at `submit.rs:121` and ends at `submit.rs:948`).

## The defect, as measured

* Inside that loop there are **54 `?` operators**. Every one of them returns from `submit_cb_inner`, which
  aborts the **entire command buffer** because a single operation was refused. (A raw character count over
  lines 121–948 returns 61 `?`; the difference is `?` appearing in comments and strings. 54 is the operator
  count.)
* Worse, the error returns **before `submit_encoded`** at `submit.rs:949`. `native` — the
  `wgpu::CommandEncoder` holding every render pass, compute pass, and native copy that already encoded
  successfully earlier in the same batch — is simply dropped. That work is silently discarded, with no
  diagnostic distinguishing it from work that never encoded.
* This is **pre-existing**, not introduced by any recent change.

## Why it matters

This is the amplification pattern that cost this project a full day. Chrome's 1121-command startup batch was
rolled back because **one** pipeline failed validation. Every later frame then referenced resources that had
evaporated, so one refused operation became a dead browser rather than one missing draw.

See also the note in `AGENTS.md` about `submit.rs:111` — a comment claiming a refused batch leaves both sides
agreeing it did not happen, citing a residency mirror that is byte accounting and silent about the id caches
that actually desynchronised. The comment is the defect's hiding place; this document is the fix.

## The fix, in five steps

1. **Precompute the advance before running the op.** Immediately after the `native_*` predicates and before
   the `match`, compute the next index:
   * `Enc::BeginRenderPass { .. }` → `find_end(ops, i, Enc::EndRenderPass)? + 1`
   * `Enc::BeginComputePass` → `find_end(ops, i, Enc::EndComputePass)? + 1`
   * everything else → `i + 1`

   This is the load-bearing part. It makes a refusal unable to leave `i` unmoved (an infinite loop) or to
   land **inside** a pass body, where the following ops are in-pass state setters that the outer loop must
   never interpret. A malformed buffer failing `find_end` **stays fatal** — that `?` is deliberate and must
   remain outside the per-op error handling.

   Inside the two pass arms, derive `end` as `next - 1` rather than calling `find_end` a second time.

2. **Delete the 15 in-arm advancement sites** and wrap the `match` so its error becomes a value rather than a
   return. The sites are 13 plain `i += 1` and 2 `i = end + 1`; two of the `i += 1` sites
   (`submit.rs:557` and `submit.rs:680`, `submit.rs:745`) are paired with a `continue`, which becomes an
   early `return Ok(())` once the `match` is wrapped. The wrap that works is an immediately-invoked closure
   with an explicit return type, so every existing `?` inside the arms keeps its meaning:

   ```rust
   let outcome = (|| -> Result<()> { match &ops[i] { /* arms, unchanged */ } })();
   ```

   Do not restructure beyond this. The value here is a surgical, verified change to an 840-line function.

3. **Accumulate the first refusal and keep going.** On a non-fatal error, record it if none is recorded yet
   and continue to the next op. After the loop, call `submit_encoded` so the work that **did** encode is
   submitted, and only then return the accumulated refusal. On a fatal error, abort the buffer as today.

   The refusal **must still reach the caller.** The point of this change is that it is *reported* rather than
   *swallowed*. This fleet has found four defects in one day that were a capability quietly not taken; do not
   add a fifth. The later partial-execution protocol makes this submitted partial command buffer a committed
   outcome, so its fence is scheduled after `submit_encoded` and before returning the refusal. That keeps the
   executor's native completion and the runtime timeline consistent.

4. **Add `GpuError::is_fatal()`** in `src/gpu/hl-gpu/src/protocol/model/error.rs`:
   * fatal (abort the whole buffer): `Panicked` — the executor is in an unknown state and its partial
     encoder must not be submitted — and `Transport`.
   * per-operation (accumulate and continue): the validation family — `Unsupported`, `Invalid`,
     `OutOfBounds`, `ResourceLimit`, `UnknownId`.
   * Write it as an **exhaustive** match, not a wildcard, so a new variant forces a decision. The decode
     family (`ShortBuffer`, `BadTag`, `BadEnum`, `Utf8`, `TrailingBytes`, `NonCanonicalBool`, `Decode`) means
     a malformed stream and belongs on the fatal side; `Kernel` also carries device-validation failures from
     `with_validation_scope` (`submit.rs:1155`) and should be justified explicitly rather than defaulted.

5. **Test it.** One 3D blit — currently refused by the executor at `blit.rs:315` with
   `Unsupported("wgpu: 1D/3D blit source")`, while the CPU oracle serves it — placed among ordinary commands.
   Assert **both** that the ordinary commands executed **and** that the refusal reached the caller.

   A refusal proves nothing without a positive control on the same path: the ordinary commands must be ones
   whose effect is readable back (a `ClearRect` to a known colour, read via `exec.read_texture`), and the
   destination must be pre-cleared to a distinct known value so "untouched" and "written" are
   distinguishable. `tests/blit_mirror.rs` is the closest existing template for the session/`runtime::submit`
   setup. Check whether `hl_gpu::runtime::submit` rolls the session back on a returned error before relying
   on reading resources afterwards.

   Watch every assertion fail before the code that satisfies it exists. If that is not possible, do mutation
   testing instead: revert each rule **individually**, never as a group, and record the matrix in the test
   file. An agent found a rule guarded by nothing precisely because they reverted individually; a shared
   failure signature would have hidden it.

## Build facts that cost an unprepared agent hours

* **`cargo check --workspace --all-targets` does NOT cover the whole tree.** Five crates declare their own
  `[workspace]` and are absent from the root members list:
  `src/surface/hl-vulkan/shim/vulkan`, `src/surface/hl-gl/shim/egl`, and
  `src/surface/hl-cuda/shim/{cuda,cudart,nvml}`. **All five depend on `hl-gpu`**, and `GpuError` lives in
  `hl-gpu`, so step 4 touches them. Check every one explicitly. A "workspace-wide" check missed a live
  construction site and broke the shared tree.
* **`hl-gpu-wgpu` tests need Metal and must run on the host**, not in the Linux VM:
  `mac cargo test -p hl-gpu-wgpu --test blit_mirror` works, takes ~12s incremental, and genuinely executes
  rather than being filtered. `mac` carries the working directory across.
* **The guest cross-linker (`aarch64-linux-gnu-gcc`) is absent in the Linux VM.** Anything triggering a guest
  cross-build must run on the host via `mac`.
* The tree is shared and other agents edit it live. If an error appears in a crate you did not touch, check
  whether it is someone else's in-flight work before diagnosing it as yours. Commit with
  `git commit --only -- <paths>`.
