# CPU execution schema

`layout.tsv` is the sole editable definition of the native block-entry prefix.
It mirrors the baked offsets in the retained C engine. Generate both language
views from this directory with:

```text
rustc generate.rs -o /tmp/hl-cpu-schema
/tmp/hl-cpu-schema .
/tmp/hl-cpu-schema --check .
```

The temporary generator binary stays outside the project and must not be committed.
The C and Rust outputs contain compile-time offset and size assertions. A field
may be appended only after comparing every emitter, trampoline, signal capture,
checkpoint codec, and fork path in the retained engine. Reordering an existing
field is an ABI break.

`certificate_valid`, `certificate_delta`, `active_authority`, `active_view_*`, and the `loop_*`
fields are AArch64 native-execution scratch. The authority values are published from the independently
authenticated run request only while the execution gate is held. They are reset
at every native run and whenever architectural state is captured into the native
layout. They are deliberately absent from execution checkpoints: fork and restore
reconstruct them as zero instead of treating them as guest architectural state.
The loop fields are dormant schema reserved for a future authenticated region;
ordinary trace admission and emission must leave them zero.
Each `loop_views` slot retains the exact authenticated envelope followed by the
owning projection view bounds, delta, and permissions; owner bounds must not be
reconstructed from the narrower envelope when publishing dirty records.
The active-view identity binds the current `memory_*` owner to the validated mapping
incarnation and run authority. It does not certify an access envelope: generated
guards must still check bounds and permissions before consuming the owner.
The append-only `read_view_publication` array extends each AArch64 cached view
with its validated write-publication policy and stable lease-local index without
moving the established four-word view ABI. `memory_write_policy` and
`memory_write_index` identify the owner currently installed in `memory_*`.

`x86_64.vector_dirty` is diagnostics-only native scratch. Diagnostics-enabled
translated blocks set it before their first vector-register write, chains carry
it, and the dispatcher clears it before every run and after the unconditional
architectural vector spill. It is never checkpointed or treated as guest state.
`x86_64.vector_upper` is the append-only architectural tail for YMM0..15 bits
255:128. It remains memory-resident across native blocks: legacy SSE preserves
it, while future VEX destination lowerings must implement the encoded-width
upper-zero rule before returning to Rust.
