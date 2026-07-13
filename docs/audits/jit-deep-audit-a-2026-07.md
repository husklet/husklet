# JIT deep dead/legacy audit — wave A (2026-07)

Scope: every tracked file under `dd-jit-darwin/` and `dd-jit/` (104 files). This was a documentation-only audit;
no runtime or build files were changed.

## Method and coverage

- Enumerated the 104 paths with `git ls-files dd-jit dd-jit-darwin`.
- Reconstructed the build graph from both manifests and `dd-jit-darwin/build.rs`.
- Followed the three unity translation units and their nested `#include` graph.
- Cross-referenced Rust public exports throughout the workspace and C static/global definitions throughout every
  unity translation unit.
- Enumerated `getenv` feature gates, preprocessor branches, reason codes, generated/config wire fields, checkpoint
  hooks and exported C/Rust ABI entry points.
- Treated a single textual occurrence as insufficient proof of dead code: unity includes deliberately allow a static
  definition in one file to be called by a later included file.

Useful reproduction commands:

```sh
git ls-files dd-jit dd-jit-darwin | wc -l
rg -n '^#include' dd-jit-darwin/src/runtime/targets/*.c
rg -n 'getenv\("[A-Z0-9_]+"\)' dd-jit-darwin/src/runtime
rg -n '^\s*#\s*(if|ifdef|ifndef|elif|define|undef)' dd-jit-darwin/src/runtime
rg -n '^\s*(pub use|pub mod|pub struct|pub enum|pub trait|pub fn|pub async fn)' dd-jit/src dd-jit-darwin/src
```

## Build/include reachability

No tracked C source is proven never-built.

- `src/runtime/targets/linux_aarch64.c` is the aarch64 unity TU and includes shared engine, aarch64 translator,
  Linux syscall/VFS, checkpoint and forkserver code.
- `src/runtime/targets/linux_x86_64.c` is the x86-64 unity TU and includes shared engine, x86 decoder/emitter,
  legacy syscall normalization, AVX/SSE/x87, Linux syscall/VFS and forkserver code.
- `src/runtime/targets/darwin_aarch64.c` includes `os/darwin/jitdarwin.c`.
- `os/linux/syscall/dispatch.c` nests `aio.c`, `event.c`, `fs.c`, `helpers.c`, `io.c`, `mem.c`, `misc.c`, `net.c`,
  `proc.c`, `ptrace.c`, `rare.c`, `signal.c`, `sysv.c`, and `time.c`; apparent zero direct target references to
  those leaf files are therefore not deadness.
- `os/linux/container/vfs.c` nests `vfs/gmap.c`, `overlay.c` and `resolve.c`.
- `os/darwin/ffi.c` is separately compiled into `libddjit_ffi.a`; `os/darwin/jail/jail.c` is separately built as
  `darwinjail.dylib`.
- `build.rs` builds all three target binaries and the jail artifact. Removing a target is an ABI/package decision,
  not dead-code cleanup: `Guest::ALL`, `Guest::jit_path`, package assembly and runtime detection consume them.

## High-confidence cleanup

### Stale checkpoint design document

`dd-jit-darwin/docs/CHECKPOINT.md:26` says **“No checkpoint/restore code exists yet.”** That is false. Live product
callers exist in `dd-cli/src/workspace.rs:149-183` and `dd-cli/src/ddjit_launcher.rs:55-98`; runtime control lives in
`dd-jit/src/runtime/runtime.rs:69-150`; the aarch64 target includes `os/linux/checkpoint.c` and dispatch polling.

- Recommendation: remove the obsolete speculative sections or rewrite the file as current architecture/limitations.
- Compatibility risk: none; documentation only.
- Performance risk: none.
- Proof before deletion: `git grep -n 'CHECKPOINT.md'` and compare every retained claim to the live paths above.

### Misleading source comment on checkpoint staging

`dd-jit-darwin/src/runtime/os/linux/checkpoint.c:175` still describes behavior as something “only a later increment”
triggers, despite the live trigger/manifest product path. Remove or update the comment with the document cleanup.

- Compatibility/performance risk: none.
- Proof: run the focused workspace checkpoint/restore journey after comment-only cleanup to ensure no accidental code
  edit is mixed into the change.

### Redundant `__attribute__((unused))` declarations

The forward declarations at `os/linux/container/state.c:268`, `:312`, `:325`, `:335`, `:344` and
`os/linux/container/vfs.c:2583` carry `__attribute__((unused))`, but all corresponding functions are called later in
the unity TU: `ckpt_pending` from syscall signal handling, the pid-map helpers from checkpoint/proc/signal, and
`container_gpid_member` from `syscall/rare.c:194`.

- Recommendation: remove only the misleading attributes, not the functions.
- Confidence: high.
- Compatibility/performance risk: none after a warning-clean unity build.
- Proof: compile both Linux unity targets with `-Wall -Wextra -Werror` after removing the attributes.

## Strong removal candidates requiring verification

### Historical diagnostic/prototype bundle

Several default-off facilities are explicitly described as feasibility experiments or worktree diagnostics and are
not part of the typed Rust launch contract:

- `IBPROF`, `VDBETRACE`, `VTHITCOUNT`, `CTXDISP`: state and tables begin in
  `engine/cache.c:392-610`, emission in `engine/stubs.c:233-566`, dispatch handling in
  `engine/dispatch.c:184-215`, and initialization in `targets/linux_aarch64.c:742-744`.
- `MAPDUMP`: dump/watcher machinery in `engine/cache.c:587-610` and `:1011-1089`, installed from
  `engine/dispatch.c:91-93`, dumped from `syscall/proc.c:539`.
- `BLKDUMP`: x86 emitted-word dump at `translate/x86_64/translate.c:3906-3921`.
- `T2DUMP`: duplicated emitted-word dumps at `translate/aarch64/translate.c:2016-2020` and
  `translate/x86_64/translate.c:3950-3954`.
- `DD_FAULTCOUNT`, `DDDBG_IMGBASE`, `DDDBG_INTERPBASE`: aarch64-only measurement/address forcing at
  `targets/linux_aarch64.c:497-506`, `:666`, `:814-815`, `:831-832`, and `:947-948`.
- `DDDBG_GPRDUMP`: aarch64 register differential state at `engine/stubs.c:15-17`; the x86 unity TU carries inert
  compatibility storage at `translate/x86_64/engine_glue.c:112-113` solely because shared code names it.

These are the best large zero-production-behavior deletion candidates: they are default-off and several poison
persistent-cache saving when enabled. They are not automatically safe to remove because maintainers may still depend
on their debugging ABI.

- Compatibility risk: medium (undocumented developer env ABI).
- Performance risk: low-to-positive when off, but code-layout changes can affect a JIT; benchmark both architectures.
- Proof: search scripts/docs/issues for each flag; build both unity targets; run the full Rust/C matrix; compare engine
  text size and standard performance corpus; archive any needed offline tooling before deletion.

### Retire completed A/B kill switches only as a measured batch

The engine retains many legacy implementations behind negative flags: `NOSTEAL1617`, `NOSTEALFAST`, `NOIBSLIM`,
`NOIRQSLIM`, `DDJIT_NOSLIMSYS`, `DDJIT_NOFASTSYS`, `NOFUTEXQ`, `NOSTITCH`, `NOTIER2`, `NOTIER2X`, `NOSMC`,
`NOEAOPT`, `NOFLAGELIDE`, `NOSSEOPT`, `NOX87OPT`, and related fine-grained x86 lowering gates. These are reachable
and their branches are not textually equivalent. Examples:

- steal/dispatch/IRQ gates initialize at `targets/linux_aarch64.c:745-755` and alter block layouts in
  `translate/aarch64/translate.c` plus persistent-cache versioning;
- syscall spill fallback is in `engine/stubs.c:59-67` and `translate/x86_64/emit.c:618-646`;
- fast syscall fallback calibrates at `translate/x86_64/emit.c:693-790`;
- legacy futex queue lives in `os/linux/thread.c:287-290` and `:830-871`;
- persistent cache records code-generation mode bits, so removing a branch requires a cache-version bump.

- Recommendation: do not remove piecemeal. Select gates whose optimized path has been default for at least one release,
  run each legacy flag against the complete correctness matrix, then delete the fallback and flag together.
- Compatibility risk: medium; some flags are operator escape hatches.
- Performance risk: high in translator/dispatch hot paths; deletion changes code layout and persistent-cache identity.
- Proof: per-flag differential correctness, cold/warm pcache tests, Chrome/GTK GUI journeys, x86 and aarch64 performance
  distributions, then explicit `PC_VERSION`/mode-bit review.

### Unify duplicated dump helpers after deciding whether to keep them

`T2DUMP` has near-identical implementations in both translators. If retained, move formatting to a shared helper;
if not retained, delete both. This is duplication, not currently dead code.

- Compatibility risk: low; diagnostic output only.
- Performance risk: minimal when disabled, but shared helper placement must not enter generated hot code.
- Proof: compare dump output for one tier-2 block on each architecture.

## Verify/keep findings

### Checkpoint/restore is live, not abandoned

Do not delete `os/linux/checkpoint.c`, `G_CKPT_POLL`, pid virtualization in `container/state.c`, deterministic placement
in memory/ELF paths, or `Runtime::checkpoint`. They are connected to current CLI and GUI freeze-on-close behavior.
The stale design document—not the implementation—is the cleanup target.

### x86 legacy translator is required

`translate/x86_64/legacy.c` is included by the x86 target and normalizes 58 x86-only legacy syscalls into the shared
aarch64-shaped syscall layer. Its name describes guest ABI, not obsolete code.

### Sync and async Rust runtime surfaces are both reachable

`Runtime::run` is used by `dd-jit/examples/run_container.rs`; async `start_into`/`output` is used by daemon, CLI and
GUI paths. `SpawnConfig::script`/`command` remains used by engine/scenario tests even though production uses typed FFI.
Removing either launch path now would reduce compatibility or test coverage.

### Generated/config ABI fields are live

`launch/wire.rs` mirrors `include/ddjit_api.h`; `nopcache`, `egress_off`, GPU and resource fields cross the Rust/C FFI
and have layout tests. The `reserved0` tail is deliberate ABI padding. Do not repurpose/remove without an ABI bump and
old-reader/new-writer tests.

### `DDJIT_FASTSYS_FORCE` is a test hook, not a redundant enable flag

At `translate/x86_64/emit.c:734-737` it bypasses host timer calibration so virtualized CI can exercise guarded fast
time paths. It is distinct from the default auto-enable and `DDJIT_NOFASTSYS` kill switch.

## Build-system cleanup opportunities

- The unity build makes ordinary per-file compiler dependency/warning reports hard to interpret. Generate and archive
  preprocessed TUs (`clang -E`) and symbol tables as audit artifacts; compile each with `-Wall -Wextra -Werror`.
- Add a build check that every tracked runtime `.c` is either a target, directly compiled artifact, or reachable in an
  include graph. The manual audit found all current files reachable, but this invariant is not automated.
- Add an exported-symbol allowlist for `ddjit_spawn`/engine entry points and the jail interposition set. This prevents
  accidental ABI retention from being mistaken for required reachability.

## Ranked conclusion

1. Clean the stale checkpoint document/comments and misleading unused attributes now.
2. Decide whether the measurement bundle (`IBPROF`/`VDBETRACE`/`CTXDISP`/`MAPDUMP`/`BLKDUMP`/`T2DUMP`) is still a
   supported developer interface; delete or consolidate it as one reviewed change.
3. Retire optimized-path A/B fallbacks only after differential correctness, pcache-version and performance gates.
4. Keep all translators, syscall family files, checkpoint implementation, typed launch ABI and both Rust runtime
   surfaces; none is proven dead.
