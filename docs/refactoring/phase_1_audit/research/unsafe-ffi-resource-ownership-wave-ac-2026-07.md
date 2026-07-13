# Unsafe/FFI/resource ownership audit — wave AC (2026-07)

Scope: current Rust/C boundaries in JIT launch, GPU/IOSurface/Metal/wgpu, display/compositor fd transport, and daemon/runtime process ownership. No code was changed.

## Confirmed correctness bugs

### IOSurface metadata leaks a +1 reference

`resolve_iosurface(id)` explicitly returns a +1 retained `IOSurfaceRef`: bridge-cache hits call `CFRetain`, and `IOSurfaceLookup` also returns an owned reference under the documented wrapper contract. `iosurface_metadata()` reads width/height/stride/format and returns without `CFRelease`. Every import/metadata validation leaks one surface reference.

Fix with one RAII `OwnedIOSurface` newtype whose `Drop` calls `CFRelease`; make resolve/create return it and expose a borrowed raw pointer only inside scoped Metal calls. This removes repeated manual `cfrelease` paths and closes early-return leaks. It is a correctness fix, not behavior-neutral deletion. Gate with repeated import/drop and live-client churn while observing IOSurface/VM/object counts.

### JIT child environment leaks in the parent

`ddjit_child_env()` allocates an env-pointer array before `fork`. Its comment says the allocation is intentionally leaked because the process execs or exits, but only the child does: the long-lived Rust/daemon parent returns from `ddjit_spawn` and leaks one array per launch whenever the variable was not already present. The strings are borrowed; only the pointer array needs freeing in the parent after `fork` succeeds or fails. Return `{envp, owned}` or a separate allocated pointer, and free only in the parent after the child has inherited its COW copy. Preserve the child’s async-signal-safe rule.

Test thousands of launches with and without an explicit variable, fork failure injection, and exec failure. This is a correctness fix.

## File-descriptor ownership

`dd-display::wire::Conn` stores its socket and SCM_RIGHTS queues as raw integers. Drop closes unconsumed received fds, but not the connection fd; outer loops manually close it. `take_fd()` transfers ownership as an untyped `RawFd`, making leaks/double-close possible. `queue_fd()` is borrowed—the sender retains its descriptor after SCM_RIGHTS—yet the type does not say so.

Safe internal refactor:

- `Conn` owns an `OwnedFd` socket and closes it on Drop;
- received SCM_RIGHTS entries are `OwnedFd`; `take_fd()` returns `OwnedFd`;
- outbound entries are borrowed descriptors for the duration of flush or duplicated owned fds if queued lifetime can exceed the owner;
- remove all matching manual socket closes only after callers convert.

This preserves wire ABI and syscall count if outbound fds remain borrowed. Do not blindly convert outbound fds to `OwnedFd`: closing a queued borrowed keymap/pool fd would change caller ownership, while `dup` would add cost and failure modes.

The JIT runtime already uses `OwnedFd` for pipes/PTys, but exposes `pty_master: RawFd` while an `Arc<AsyncFd<OwnedFd>>` owns it. The raw handle can outlive the async owner. Return a borrowed view tied to the handle or a duplicated `OwnedFd`; document whether callers may close it (currently they must not).

Compositor tests contain `OwnedFd::from_raw_fd(child_fd.as_raw_fd())`, creating two owners of one fd before `_exit`. The process boundary makes the test often survive, but the construction violates Rust ownership and is a bad pattern to copy. Use `dup` or transfer with `into_raw_fd`.

## IOSurface/Metal fence scaffolding

The global bridge and fence maps store Objective-C/CoreFoundation pointers as `usize` to bypass `Send`. Fence events are intentionally converted with `Retained::into_raw` then reconstructed in `fence_drop`. This is balanced when `fence_drop` is called, but process-lifetime/error paths can retain entries indefinitely, and the integer erases ownership/thread constraints.

Keep the cross-thread cache—it is required for zero-copy and tearing fences—but replace naked integers with small unsafe `Send` wrappers that state the invariant: IOSurface/MTLEvent references are retained, thread-safe for the used API, and released in Drop. Then map removal is ordinary RAII and partial event creation cannot leak one event when the second fails. Do not remove fences or the bridge-cache fallback; they are compatibility/performance mechanisms.

`start_gpu_bridge` retries forever after every receive error. Repeated permanent Mach-port failure can spin/log forever. Classify interrupted/transient receive errors versus invalid/dead service errors; terminate and mark GPU health failed on terminal errors.

## Validation boundaries

Several apparent duplicate checks defend different authorities and must remain:

- Rust wire layout checks protect serialization; C configfd validates untrusted length/magic/ABI/offsets before dereference.
- compositor dmabuf import validates modifier/fd/dimensions; presenter resolution validates that the host IOSurface still exists and matches at presentation time.
- Smithay validates pool shape; compositor checked arithmetic validates client-controlled offset/stride/height before unsafe pointer copies.
- GPU decoder validates stream framing; backend validates resource state/capabilities.

Remove duplicated validation only when both checks use the same immutable facts at the same trust boundary. Import-time and use-time checks are not duplicates because resources can disappear or IDs can be reused.

One worthwhile consolidation is a shared checked image-layout function for offset + row-stride + width × bytes-per-pixel. Both compositor shm copy and display/backend upload paths should consume the result rather than repeat unsafe pointer arithmetic. Keep validation at each boundary while sharing the arithmetic implementation.

## JIT FFI/process resources

Rust correctly keeps config bytes and engine CString alive across `ddjit_spawn`; C copies the config to a private file before fork. The C shim owns/unlinks that file on pre-fork failures, and the engine opens/unlinks it on success. Audit the child exec-failure branch to unlink the file before `_exit`; otherwise a bad engine/codesign/path accumulates private config files. Parent cannot safely unlink immediately because the child opens by path.

The raw fd parameters are borrowed across the call and duped only in the child, matching Rust’s safety comment. Do not wrap them as transferred ownership. Validate negative/inconsistent TTY fd combinations in Rust before FFI so C does not enter a partially configured child path.

The C config wire uses borrowed pointers only during `dd_run_configfd` and frees pool/argv storage if `dd_run` returns. Verify no engine subsystem stores pointers into the config pool past initialization; guest argv/env consumers that persist must copy.

## GPU executor/backend ownership

wgpu objects are RAII and should remain so; raw IOSurface texture descriptors are the exceptional boundary. A texture created from IOSurface must not outlive the retained surface unless Metal explicitly retains it. Encode this dependency in one wrapper holding both texture and surface owner.

Device polling, map callbacks, and readback buffers need typed completion ownership: callback state must be completed/cancelled exactly once on device loss. Avoid `mem::forget`/process-lifetime leaks as synchronization shortcuts. No redundant wgpu ownership wrapper is proven safe to delete.

## Daemon/runtime tasks

The JIT runtime owns child PIDs, pump tasks, and pipe fds across separate structs. Timeout kills/reaps the process but bounded task joins can leave tasks detached; channels eventually close, yet ownership is implicit. A single child-session RAII object should own pid/pgid, fds and join handles, with explicit `wait`, `kill_and_reap`, and Drop fallback. Drop cannot block async executors, so use an owned reaper task rather than synchronous `waitpid` in Drop.

Ignored channel sends during shutdown are safe: receiver closure means no owner remains. Ignored task/pump errors are not ownership leaks but should feed the truthful outcome work from wave Y.

## Safe cuts versus fixes

Behavior-neutral cuts after RAII conversion:

1. remove manual `CFRelease` branches replaced by `OwnedIOSurface` Drop;
2. remove manual connection-fd closes replaced by `Conn<OwnedFd>` Drop;
3. remove raw `usize` retain/from-raw bookkeeping replaced by owning fence wrappers;
4. factor repeated checked image-layout arithmetic while retaining boundary checks;
5. remove redundant outer cleanup branches only after ownership tests prove exactly-once release.

Correctness fixes, not neutral deletion: metadata CF leak, parent env-array leak, exec-failure config residue, terminal Mach receive loop, raw PTY lifetime, and test double ownership.

## Acceptance

Run Miri where libc/Objective-C is not involved and sanitizers/leak tools on macOS for native boundaries. Add Rust/C behavioral tests for SCM_RIGHTS queued/partial-send/disconnect ownership, fd-number reuse, repeated IOSurface import/drop, fence partial creation/client churn, 10k JIT spawns, fork/exec failure, config cleanup, PTY handle lifetime, malformed wire/dmabuf layouts, device loss and compositor buffer release. Build all cfg targets, especially the mac-only display/compositor/wgpu crates. Measure syscall count and frame/launch latency to ensure RAII consolidation adds no `dup`, retain/release, lock, or allocation on hot paths.
