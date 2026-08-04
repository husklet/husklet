# Retained C compatibility inventory gap

This report compares the live read-only C oracle manifests with the Rust normalized build plan.
Counts are case/ISA legs, not CTest umbrella registrations.

## Summary

- C manifest legs: 3101.
- C macOS-active legs: 2954.
- C Linux-active legs: 3073.
- Rust build-plan rows: 3131.
- Rust execution inventory rows: 3113.
- `exact`: 2954.
- `excluded-retained`: 147.
- Rust-only `bootstrap`: 10.
- Rust-only `rust-local`: 30.

Every live retained-C case/ISA leg is represented: all 2,954 macOS-active
legs match exactly and all 147 non-active-on-macOS dispositions are retained.
The Linux denominator is separately preserved at 3,073 active legs; its 119
additional legs carry `excluded-macos`, not a Linux exclusion.
Nested ABI/core/ISA suites, the legacy ABI schema, and soak therefore have
zero inventory omissions.

## Per-suite C legs

| C suite | Legs | Exact | Excluded retained | Excluded missing | Renamed | Consolidated | Source changed | Missing |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `abi` | 146 | 144 | 2 | 0 | 0 | 0 | 0 | 0 |
| `abi/corpus` | 263 | 263 | 0 | 0 | 0 | 0 | 0 | 0 |
| `completeness` | 275 | 273 | 2 | 0 | 0 | 0 | 0 | 0 |
| `core/abi` | 62 | 62 | 0 | 0 | 0 | 0 | 0 | 0 |
| `core/regress` | 23 | 23 | 0 | 0 | 0 | 0 | 0 | 0 |
| `core/syscall` | 115 | 113 | 2 | 0 | 0 | 0 | 0 | 0 |
| `core/workload` | 32 | 32 | 0 | 0 | 0 | 0 | 0 | 0 |
| `filesystem` | 177 | 167 | 10 | 0 | 0 | 0 | 0 | 0 |
| `ipc` | 248 | 235 | 13 | 0 | 0 | 0 | 0 | 0 |
| `isa/aarch64` | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| `isa/x86_64` | 8 | 8 | 0 | 0 | 0 | 0 | 0 | 0 |
| `isolation` | 42 | 42 | 0 | 0 | 0 | 0 | 0 | 0 |
| `libc` | 190 | 190 | 0 | 0 | 0 | 0 | 0 | 0 |
| `memory` | 211 | 177 | 34 | 0 | 0 | 0 | 0 | 0 |
| `network` | 174 | 154 | 20 | 0 | 0 | 0 | 0 | 0 |
| `posix` | 195 | 181 | 14 | 0 | 0 | 0 | 0 | 0 |
| `process` | 164 | 144 | 20 | 0 | 0 | 0 | 0 | 0 |
| `procfs` | 114 | 104 | 10 | 0 | 0 | 0 | 0 | 0 |
| `signals` | 135 | 133 | 2 | 0 | 0 | 0 | 0 | 0 |
| `soak` | 33 | 31 | 2 | 0 | 0 | 0 | 0 | 0 |
| `syscall` | 176 | 162 | 14 | 0 | 0 | 0 | 0 | 0 |
| `syscall_edges` | 104 | 102 | 2 | 0 | 0 | 0 | 0 | 0 |
| `threads` | 134 | 134 | 0 | 0 | 0 | 0 | 0 | 0 |
| `time` | 79 | 79 | 0 | 0 | 0 | 0 | 0 | 0 |

## Classification contract

- `exact`: case, declared source, and ISA match a Rust build-plan row.
- `renamed`: declared source and ISA match exactly, but the case name differs.
- `consolidated`: one retained source/ISA maps to multiple Rust rows and requires review.
- `source-changed`: case and ISA match but the Rust source identity differs.
- `missing`: no case or source identity exists in the Rust build plan.
- `excluded-retained` / `excluded-missing`: the C disposition is non-active, separated by whether Rust retained it.
- `bootstrap`: an execution-inventory seed with no imported build-plan row.
- `rust-local`: an explicit Rust-owned overlay row absent from the live retained C manifests.

The row-level machine-readable evidence is `C_INVENTORY_GAP.tsv` beside this document.

## Reproduce

```sh
cd /Users/x/dd/engine_rust
python3 src/tests/compat/inventory_gap.py --engine /Users/x/dd/engine
```

The command reads all recursively discovered C `manifest.tsv` files using the 7-column legacy and 13-column current schemas accepted by `tools/matrix_runner.c`. It does not build guests or run Cargo.
