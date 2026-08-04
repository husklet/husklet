# Syscall core migration evidence

## Boundary

This folder's retained `core/syscall` subset contributes 43 cases and 85 ISA
rows; the complete retained manifest has 58 cases and 115 rows, with the other
registrations already owned by focused runtime categories.
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

- 83 active rows compiled with the retained `-static-pie -O2 -pthread -lm`
  contract (`fd-shadow-contention` intentionally omits `-lm`);
- all 83 active rows passed QEMU with byte-exact stdout and exit status 0;
- two `mprotect` rows were skipped with the manifest's explicit unsupported
  status;
- both `mprotect` sources were then compiled manually with the same per-ISA
  flags and passed their QEMU golden output, proving the oracle fixture itself
  is sound while preserving the separate macOS product exclusion;
- all 85 expected executable artifacts were produced under the generated target
  directory.

The final two active rows are retained case `scmrights`. Its child sends an
open file description over an AF_UNIX socket and closes its original
descriptor before the parent receives it. Both guest ISAs returned
`scmrights got_fd=1 data=fd-passed-ok`, proving the native oracle retained the
descriptor and its file offset across the handoff.

Compiler warnings originate in the retained byte-exact C sources and were not
“fixed” by changing migration evidence.

## Integrity and repository scope

- 43 C files and 43 golden files were compared against their mapped retained
  originals; every SHA-256 digest matched.
- The category contains only sources, goldens, `test.yaml`, and these audit
  documents. Generated binaries and result tables remain under `target/`.
- No shared inventory, migration ledger, or source package was edited by this
  lane.
- The retained `../engine` tree was not modified.

This proves migration and Linux-QEMU oracle fidelity. It does not yet prove all
85 rows through the Rust product engine; that is the next acceptance boundary.
