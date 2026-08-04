# Core ABI verification evidence

Verification is recorded against the current shared tree without claiming a
committed-tree gate. The authoritative cohort is 30 cases and 52 ISA rows from
`test.yaml`; sources and goldens were compared byte-for-byte to the retained
read-only manifest before compilation.

## Contract audit

The manifest audit loaded `test.yaml` and compared it with
`../engine/tests/compat/core/abi/manifest.tsv`:

- cases: 30/30;
- target rows: 52/52 (23 Arm64, 29 AMD64);
- unique stable IDs: 30/30;
- target sets, compiler flags, exit codes, and 120-second deadlines: 30/30;
- source bytes: 30/30 exact;
- golden bytes: 30/30 exact, including the no-final-newline `atexit.out`;
- filename policy: no dash and at most one underscore in every C/golden stem;
- category contents: source, golden, YAML, oracle audit, and this evidence only;
  no prebuilt executable or result capture.

## Compiler and QEMU oracle

The repository runner was invoked with its 18-worker default for each ISA:

```text
target/debug/testing oracle abi_core --isa arm64
target/debug/testing oracle abi_core --isa amd64
```

Results:

- Arm64: 23/23 compiled and passed QEMU exit/stdout comparison;
- AMD64: 29/29 compiled and passed QEMU exit/stdout comparison;
- combined: 52/52 passed, zero timeouts and zero output mismatches.

The run emits retained-source `warn_unused_result` compiler warnings in
`atexit.c`, `files.c`, and `statfile.c`; these are oracle-source warnings, not
execution failures, and changing them would violate byte identity.

An attempted rebuild of the Rust runner was blocked by an unrelated concurrent
shared-tree error: `hl-engine` derives `Debug` for a field containing
`Arc<hl_runtime::SystemAuthority>`, while `SystemAuthority` does not implement
`Debug`. The existing repository runner was used after that failure; it parsed
the current category and freshly cross-compiled every current C source before
running QEMU. No claim is made here about a clean committed Rust build.
