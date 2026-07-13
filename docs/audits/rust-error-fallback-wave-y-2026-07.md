# Rust error and fallback audit — wave Y (2026-07)

Scope: current core/backend/runtime Rust crates, including JIT launch/runtime, GPU core/wgpu, display/compositor, and their macOS cfg variants. This separates truthful failure fixes from behavior-neutral deletion. No code was changed.

## Result

Most ignored results fall into three legitimate classes: cleanup after the owning resource is already gone, best-effort diagnostics/dumps, and channel sends during teardown. They should not be mechanically converted to fatal errors. The important problems are at product boundaries where an operation reports success after setup, transport, persistence, or presentation failed.

Only one fallback is proven behavior-neutral to delete immediately: `dd_jit_darwin::available()` repeats an existence check already performed by `Guest::jit_path()`. Replace the second `Path::exists` map with `jit_path().is_some()`. This does not close the unavoidable path-removal race; `spawn` remains the truthful authority.

## Truthful failure fixes

### JIT runtime

- `Runtime::with_defaults` ignores failure to create the configured pcache directory, then still enables `DDJIT_PCACHE` with that path. This converts operator/storage failure into engine-side fallback or later noise. Because `with_defaults` cannot return `Result`, either validate/create the directory in `Runtime::new/cache_dir`, make default application fallible, or disable pcache with an explicit warning. Do not silently claim it is enabled.
- `bump_trigger` ignores parent-directory creation. The following `open` eventually returns an error, so swallowing adds no compatibility. Propagate `create_dir_all` failure directly; this is a truthful-error fix and improves attribution.
- checkpoint retry kicks ignore every `kill` error until timeout. `ESRCH` means the target is already gone and should fail immediately unless a complete manifest exists; permission/invalid-signal errors are terminal. Transient re-kicks can remain best effort only after classifying errno.
- `output()` returns `(-1, bytes)` on timeout and discards reap/join outcomes. `-1` conflates timeout with a real signal/exit representation, and pump timeout/error can yield truncated output with `Ok`. Preserve the existing convenience API if compatibility demands it, but add a typed outcome (`Exited`, `Signaled`, `TimedOut`, `IoFailed`) and make the strict path surface pump failures.
- stdin pump treats all write errors as normal closure. `EPIPE`/peer close is expected; other errors should reach the run handle/log rather than vanish.

Cleanup of old checkpoint directories and trigger/pid files is intentionally idempotent. `NotFound` is benign; permission/I/O failures should be logged when they can leave a stale generation that affects the next launch.

### Compositor/display

- the Smithay compositor’s repack-cache allocation rollback ignores `replace_cache_charge` failure. If the accounting API can fail while reducing/restoring charge, the budget can drift after allocation failure. Make release/rollback infallible or assert/report invariant failure; do not merely propagate a second boolean.
- numerous Wayland `conn.flush().ok()` calls suppress disconnect and protocol transport errors. In teardown/test drains this is fine; in live request handlers it can leave clients believing state was delivered. Centralize flush policy: `WouldBlock` queues/retries, disconnect removes the client, other I/O terminates that connection with diagnostics.
- Cocoa live loops use `let _ = server.pump()` or combine `Ok(false) | Err(_)` into the same stop path. Stopping is reasonable, but errors need to be logged/returned so a renderer failure is not indistinguishable from orderly EOF.
- ignored PNG/debug/profile writes are correctly best effort, but selftest outputs are test artifacts: their write/rename failures must fail the selftest rather than produce a false pass.

The compositor’s retryable-versus-terminal presentation state machine is live correctness machinery. It retains buffers and feedback on retryable failures and must not be cut merely because it is a fallback.

### GPU backend/core

- wgpu `blit_pipeline_for`, `clear_pipeline_for`, and `flip_pipeline_for` fall back to an RGBA8 pipeline when the requested format is absent. Pipeline target format must match the render attachment; this fallback is not compatibility and can trigger validation failure or wrong output. Ensure the requested pipeline first, or return a typed unsupported/creation error. Never substitute another format.
- missing shader IDs and failed shader translation currently select builtin WGSL pipelines. This is an explicit legacy compatibility path used by examples/tests, not behavior-neutral. Narrow it to payloads positively identified as the legacy shim shape; malformed or unsupported advertised shader payloads must return `Unsupported` rather than render a plausible but wrong image.
- cached shader-translation failure avoids duplicate expensive retries and should remain. Cache the typed reason as well as the hash so diagnostics stay truthful.
- `device.poll(...Wait)` results are ignored in several synchronization/readback paths. Confirm wgpu’s return type/semantics for the pinned version; device-lost/out-of-memory should surface at submission/readback boundaries rather than turn into stale output.
- stale overlay-stub pruning deliberately skips unreadable entries and logs deletion failures. That is repair tooling, not launch authority; retaining the guest’s normal loader behavior is safer than failing launch.

## Behavior-neutral cuts

1. Remove `available()`’s duplicate existence check.
2. Replace ignored parent creation in `bump_trigger` with direct `?`; the later open already fails for the same condition, so no successful behavior is lost.
3. Remove fallback branches that only restate an invariant after making the invariant explicit: cache-charge release should be infallible, and a format-specific pipeline lookup after `ensure_*` should be an invariant check rather than RGBA substitution. The latter requires call-graph proof for every format before deletion.
4. Cleanup `remove_file/remove_dir_all` helpers may share one “ignore NotFound, report other errors” function. This consolidates policy; it must not make destructor/drop paths panic.

## Fallbacks to keep

- engine path resolution order (explicit directory, executable sibling, baked build path) is packaging compatibility, not duplicate retry;
- CPU GPU executor/readback paths are correctness/headless fallbacks;
- shader legacy fallback remains until the shim payload contract is removed end-to-end;
- compositor retryable presentation and queued nonblocking wire flush are required protocol behavior;
- poisoned mutex recovery in GPU limits preserves accounting access after panic and is preferable to cascading poison failures;
- optional environment/default-path parsing and Dockerfile shell-form fallback are user-input compatibility.

## cfg/target validation

Plain workspace builds do not compile `dd-display`, `dd-compositor`, or `dd-gpu-wgpu`. Any fix touching shared presentation/GPU errors must run the macOS crate gate. Also validate Linux-host compilation of the JIT API because checkpoint signal constants and macOS execution are intentionally separated by runtime assumptions rather than all being cfg-elided.

Required gates:

- Rust unit tests for directory permission failure, stale checkpoint artifacts, ESRCH during checkpoint, timeout versus signal, pump EPIPE versus real I/O failure;
- compositor tests for retry, terminal failure, client disconnect, cache-budget rollback and selftest artifact write failure;
- wgpu/Metal tests across every advertised color/depth format, missing pipeline creation, malformed shader, legacy shader, device loss and readback failure;
- `cargo test` for default members plus the macOS renderer/compositor/wgpu gate and both JIT guest targets.

Tests must exercise outcomes and side effects, not search source for `ok()` or fallback strings.

## Maximal groups

**Y1, safe cleanup:** duplicate `available` check, direct parent-create error propagation, common idempotent-cleanup policy.

**Y2, JIT truthful outcomes:** typed process/output result, classified checkpoint retry errors, pump error propagation. Compatibility-sensitive API change; add strict APIs before changing existing callers.

**Y3, presentation truth:** central flush/pump policy, infallible budget rollback, selftest write failures. Preserve retryable frame ownership.

**Y4, GPU truthful fallback:** eliminate cross-format pipeline substitution and restrict builtin shaders to identified legacy payloads. Validate all backend targets before removing any legacy path.
