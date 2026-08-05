# AArch64 fallback attribution

The retained diagnostic ownership was audited in
`../engine/src/core/dispatch.c::run_guest` and the AArch64 translation refusal
paths in `../engine/src/translator/guest/aarch64/translate.c`. Retained code
keeps diagnostics outside signal handlers and attributes dispatcher reasons
after architectural state is fully published. The Rust owner is
`src/executor.c::run_aarch64`; cache-build declines and guard-resolver exits are
the two bounded points where the refusal category is known without logging a
guest PC or retaining guest data.

The ABI extension appends six monotonic counters, preserving the complete
336-byte prefix. Guard read/write counters identify projection resolution.
Terminal declines are categorized as SIMD/FP, memory, control/system, or
other from the fixed instruction word. Counters are atomic because executor
instances admit concurrent guest threads. They are initialized, incremented,
read, and printed only when native diagnostics are enabled; ordinary execution
does no attribution work.

These are causal categories, not capability claims. In particular, a guard
fallback means a translated memory operation needs a new projection; it does
not mean the memory opcode is unsupported. Terminal family counters identify
where a complete retained family audit should begin before implementation.
