# Syscall core migration evidence

## Boundary

The retained `core/syscall` inventory contributes 42 cases and 83 ISA rows.
Every case targets AArch64 and x86-64 except `clockabstime`, which is AArch64
only. IDs, target selection, exit status, timeout, compiler flags, and empty
argument/environment contracts are preserved in `test.yaml`.

The source and golden names were normalized to the repository filename rule;
their bytes were not edited. `splice`, `typed-sendfile`, and `typed-vector-io`
retain their scratch-rootfs execution contract through the isolated Alpine
image. `typed-filelock` retains its multiprocess contract in its self-contained
source. `getdents` retains its executable-provided directory setup. No selected
row requires an external special device or network fixture.

`mprotect` remains typed `unsupported` for the retained `excluded-macos`
disposition. It is not silently counted as a product pass.

## Oracle execution

Command:

```text
HL_COMPAT_JOBS=18 target/debug/testing oracle syscall_core --check --jobs 18 \
  --results target/testing/runtime/syscall_core_oracle.tsv
```

Result:

- 81 active rows compiled with the retained `-static-pie -O2 -pthread -lm`
  contract (`fd-shadow-contention` intentionally omits `-lm`);
- all 81 active rows passed QEMU with byte-exact stdout and exit status 0;
- two `mprotect` rows were skipped with the manifest's explicit unsupported
  status;
- both `mprotect` sources were then compiled manually with the same per-ISA
  flags and passed their QEMU golden output, proving the oracle fixture itself
  is sound while preserving the separate macOS product exclusion;
- all 83 expected executable artifacts were produced under the generated target
  directory.

Compiler warnings originate in the retained byte-exact C sources and were not
“fixed” by changing migration evidence.

## Integrity and repository scope

- 42 C files and 42 golden files were compared against their mapped retained
  originals; every SHA-256 digest matched.
- The category contains only sources, goldens, `test.yaml`, and these audit
  documents. Generated binaries and result tables remain under `target/`.
- No shared inventory, migration ledger, or source package was edited by this
  lane.
- The retained `../engine` tree was not modified.

This proves migration and Linux-QEMU oracle fidelity. It does not yet prove all
83 rows through the Rust product engine; that is the next acceptance boundary.
