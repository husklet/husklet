# JIT unity-aware C symbol reachability — wave U (2026-07)

Scope: internal C symbols across the three real unity translation units, later textual includes, callbacks, function pointers, signal handlers, and exported/interposed entry points. Known diagnostic and syscall-201 duplicates are excluded. No code was changed.

## Method and result

Each target is one unity TU. Per-file grep is therefore unsound: a `static` definition may be called from a later included file, named by a macro, installed as a callback, or live only in one target. I compiled every real target with Clang semantic unused analysis:

```sh
for t in linux_aarch64 linux_x86_64 darwin_aarch64; do
  mac clang -O2 -Wall -Wextra -Wunused-function -Wunused-variable \
    -Wredundant-decls -fsyntax-only dd-jit-darwin/src/runtime/targets/$t.c
done
```

Cross-checking warnings against whole-runtime references proves four definition-only private functions common to both Linux engines:

| Symbol | Definition and proof | Safe action |
|---|---|---|
| `txln_has` | `engine/cache.c`; unused in ARM and x86. Other mentions are comments; active SMC uses `txln_flush_class` | Delete; update comments to the active classifier |
| `add_pend` | `engine/cache.c`; unused in both. `add_pend2/3` and their callers are live | Delete wrapper; replace obsolete comment shorthand |
| `synth_stat` | `container/vfs.c`; unused in both, while comments still claim proc/stat paths call it | Delete and repair comments; behavior-test synthetic proc/sys stat |
| `ipc_ns_key` | `syscall/helpers.c`; unused in both, while netns comments claim IPC isolation uses it | Delete; separately audit the apparent IPC namespace hole |

These are behavior-neutral source cuts. Binary savings may be zero because `-O2` can already omit unused static functions. The maintenance gain is removal of false architectural claims.

One x86 local `mem` at `translate/x86_64/translate.c:2502` is semantically unused. Remove it after inspecting that decode case; this is variable cleanup, not path deletion.

## False-positive protections

Many functions warned only in x86 are live in ARM: SMC helpers, ARM-B1 hooks, tiering, signal routing and translator helpers. `ckpt_place_bump_past` is called by the ARM checkpoint path; `mach_async_fault_signal` is called by the ARM target. Do not delete a symbol based on one target warning.

Reference counts also miss or misclassify:

- signal handlers, pthread starts, qsort comparators and atexit callbacks;
- Mach callbacks, target `G_*` macros and functions whose addresses are emitted into guest code;
- persistent-cache relocation targets;
- `main`, `dd_run`, FFI `ddjit_spawn`, dyld-interposed jail functions and dlsym/name-based exports;
- declarations required solely by unity include order.

Clang recognizes address-taken static callbacks. It cannot prove string/dyld/FFI external reachability, so exported-symbol inspection remains mandatory.

## Globals, declarations and attributes

`-Wunused-variable` found no common definition-only static global in both Linux targets beyond the x86 local. Large orphaned arrays from waves D/M remain referenced by their diagnostic features and must be deleted with those features, not individually.

`-Wredundant-decls` produced no redundant-declaration warning in the three unity TUs. Existing forward declarations bridge include order and are not safe deletions based on proximity. Attributes such as `used`, visibility, constructor/destructor, format, noreturn, alignment, weak/interpose and TLS define ABI or optimizer behavior; this pass proves no attribute-only deletion.

The Darwin jail is a separate dylib. Its syntax pass reports unused parser helpers from shared `container_parse.h`; those are live in Linux. Prefer `static inline` or a split header API over deleting Linux functionality.

## Stale comments and missing behavior

Comments around `synth_stat` still name it as the synthetic proc/sys provider. After deletion, name the active path and test `/proc/self`, numeric pid/task entries, synthetic proc files and relevant `/sys` nodes.

The `ipc_ns_key` comments claim SysV/POSIX IPC keys are container-isolated, but the function has no caller. Deleting it preserves behavior while exposing a possible isolation defect. Test `shmget`, `semget`, `msgget` and POSIX mq names across two containers before claiming isolation.

Replace `txln_has` references with `txln_flush_class`, and replace generic `add_pend` references with `add_pend2/3` or “pending-link machinery.”

## Maximal safe group U1 and proof

Delete the four functions, the inspected unused local, and comments specifically tied to those obsolete symbols. Do not combine this with target-specific warnings or exported-symbol changes.

```sh
for t in linux_aarch64 linux_x86_64 darwin_aarch64; do
  mac clang -O2 -Wall -Wextra -Werror=unused-function \
    -Werror=unused-variable -Werror=redundant-decls -fsyntax-only \
    dd-jit-darwin/src/runtime/targets/$t.c
done

# On macOS, build with a temporary -Wl,-map,<file>, then inspect:
mac nm -m -gU <engine>
mac otool -l <engine>
mac size -m <engine>
```

Inspect the jail dylib exports separately with `nm -m -gU`. Run both Rust/C guest matrices plus synthetic proc/sys stat, SMC saturation/content, direct-branch stitching, and cross-container IPC isolation tests. Source grep alone is not acceptance proof.
