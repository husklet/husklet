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

`certificate_valid`, `certificate_delta`, `active_authority`, and the `loop_*` fields are AArch64
native-execution scratch. The authority value is published from the independently
authenticated run request only while the execution gate is held. They are reset
at every native run and whenever architectural state is captured into the native
layout. They are deliberately absent from execution checkpoints: fork and restore
reconstruct them as zero instead of treating them as guest architectural state.
The loop fields are dormant schema reserved for a future authenticated region;
ordinary trace admission and emission must leave them zero.
Each `loop_views` slot retains the exact authenticated envelope followed by the
owning projection view bounds, delta, and permissions; owner bounds must not be
reconstructed from the narrower envelope when publishing dirty records.
