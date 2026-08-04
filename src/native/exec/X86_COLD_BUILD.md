# X86 cold-build admission

This audit was performed against Husklet `a38d60628fd6339940148b222f6e7947b19b2d27`
and the read-only retained engine at `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.

## Retained implementation

The complete retained ownership path inspected was
`../engine/src/translator/cache.c` (`map_idx`, `map_put`, `jit_wprot`,
`jit_publish_code`, `jit_flush_to_fresh`, stop-the-world admission and retired
arena reclamation) and `../engine/src/translator/guest/x86_64/translate.c`
(`translate_block`, `tier2_promote`, direct/indirect chain emission and the
dispatcher miss path).  The dispatcher serializes writers with `g_jit_lock`.
It forms one bounded superblock, writes through the RW alias, completes the
instruction-cache publication before inserting its exact starting PC in the
map, and only then patches pending edges.  Readers use published RX aliases.
Cache rollover parks translated peers, installs a fresh generation, and retains
old mappings until no registered CPU can execute them.  Translation has no
guest-visible partial result, errno, cancellation, or blocking result; a miss
occurs only while guest state is fully spilled at the dispatcher boundary.
Architecture-specific code emission is AArch64; host publication has Linux,
macOS, and Windows W^X adapters.

## Rust comparison

`src/native/exec/src/arch/x86_64/run.c` owns x86 miss admission and
`emit_block`; `src/native/exec/src/translation.c` owns serialized reserve,
RW-copy, publication, relocation, and rollback; `cache/cache.c` owns exact-key
lookup, generations, overlap invalidation, and retained arena identity;
`src/executor.c` owns mutation/execution leases and rollover.

The publication lifecycle is equivalent, but one public `hl_native_run` could
perform an unbounded number of cold translations while following dynamic
branches.  Its guest instruction budget charged executed instructions only.
Consequently a large scheduler slice could spend its entire wall-time budget in
cold compilation without exposing a safe scheduler boundary.  Internal source
resolution also bypassed the higher-level two-observation admission policy.

## Contract

An x86 public invocation may attempt at most 64 cold builds.  At the next miss
it returns the existing typed `YIELD` boundary before decoding, publishing,
or executing that PC.  All earlier blocks have committed guest state normally;
the current PC and remaining guest budget are unchanged.  The runtime can
execute that instruction through its interpreter and later re-enter native
execution, so no instruction is replayed and no application identity is used.
Warm cache hits remain unlimited and the quota adds no clock read unless typed
diagnostics are enabled.

`test/x86_continue.c:cold_dynamic_chain_is_bounded` supplies a deterministic
66-target indirect chain.  It proves the first invocation publishes exactly 64
blocks, reports one quota exit at the 65th PC with exact executed/budget state,
and a later invocation continues from that boundary. Diagnostics expose total
cold builds and quota exits; wall time is measured by the Rust compatibility
runner around the public invocation rather than adding a clock read to the
native miss path.
