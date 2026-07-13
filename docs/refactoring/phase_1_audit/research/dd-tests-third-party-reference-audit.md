# Dead/legacy audit: tests, vendor, and reference trees

Audit date: 2026-07-12. This is documentation only; it authorizes no deletion.

## Exact scope and method

The companion [classification manifest](dd-tests-third-party-reference-files.tsv) contains one row for every path from:

```sh
git ls-files dd-tests third_party reference
```

Coverage is **1,460 / 1,460 tracked entries** (inventory SHA-256 `9b31eecc4a857db51c200b1b9c76239c26f374b4059b57d3416c8bef58e31492`):

| Scope | Entries |
|---|---:|
| `dd-tests/` | 1,097 |
| `third_party/` | 268 |
| `reference/` | 95 |

Classification totals are 18 `REMOVE_HIGH`, 1,016 `KEEP_ACTIVE_TEST`, 63 `KEEP_GATED_INDIRECT`, 268 `KEEP_VENDOR_REQUIRED`, and 95 `KEEP_REFERENCE_PINNED`.

Consumer verification covered root and crate Cargo manifests, the Makefile, `dd-tests` Rust harness/case registration, guest Makefiles and scripts, gate-invariant tests, documentation references, gitlink modes, and literal filename references. Filename absence alone was not treated as orphan evidence because Make variables and grouped case builders construct paths indirectly.

## Remove: high confidence

The benchmark island is separable from correctness testing and is the clearest maintenance-only removal:

- `dd-tests/src/bin/bench.rs`, `dd-tests/src/bench_gates.rs`, and all 16 files under `dd-tests/guests/bench/`.
- Consumers are confined to `make bench`, the auto-discovered Cargo binary, `dd_tests::bench_gates`, and benchmark-only assertions in `dd-tests/tests/gate_invariants.rs`.
- Removal must be atomic with the `Makefile` target, `dd-tests/src/lib.rs` export, benchmark-only invariant tests, and stale rebrand references. It must not remove the separate correctness matrix's optional `PERF` mode.
- `docs/refactoring/phase_3_rebrand/research/dd-gpu-frontend-inventory.md` still names `bench.rs`, `BENCH_N`, `BENCH_K`, output columns, and the auto binary. Those entries become stale with removal.

## Verify before removal

- `dd-tests/guests/arm/go_cgo_stackgrow.go`, top-level `es2*.c`, `gpu_dmabuf_client.c`, and `lse_atomics.c` have no literal basename consumer. Verify generated/group registration and historical build recipes before deletion; absence from literal search is not sufficient.
- The 63 `dd-tests/guests/gui_matrix/` entries are intentionally classified as indirectly gated. `dd-tests/tests/gate_invariants.rs` enumerates every `.c` file and requires its stem to occur in the GUI Makefile or the explicit exclusion table, currently empty. Do not call these orphaned merely because scripts do not spell every filename.
- `dd-tests/guests/shader_translate/run_shader_translate.sh` has no basename consumer. Confirm whether it is a maintainer entry point or superseded by Rust rendering-backend tests.
- `reference/moltenvk/` is a curated 89-file source slice, not a build dependency. Its `DD-README.md`, root reference README, and lock file must justify each retained slice; otherwise prefer a reproducible revision plus extraction recipe over manually maintained copied source.
- The four reference gitlinks (`reference/alacritty`, `reference/criu`, `reference/wezterm`, `reference/zluda`) are pinned reference inputs, not runtime dependencies. Verify each still informs an active plan before paying clone/submodule maintenance cost.

## Keep

- `third_party/smithay-0.7.0/` is the root workspace's direct path dependency and `dd-compositor` enables `wayland_frontend` with defaults disabled. Keep the vendored crate as a coherent Cargo package; deleting apparently unused modules individually risks cfg/build-script breakage and makes upstream comparison harder.
- Smithay's Linux DRM/GBM/X11/session/udev backends are disabled for dd, but are feature-gated upstream source rather than proven dead files. A size-reduction fork would need an all-target compile matrix and a documented update procedure; it is not a safe file-pruning task.
- `dd-tests` Cargo runners, Rust cases, scenario scripts, compliance fixtures, tools, and non-benchmark guests remain correctness/product coverage unless a concrete consumer audit proves otherwise.
- Generated-looking guest artifacts remain tracked fixtures when referenced by build scripts or byte-level tests. Do not regenerate or delete them based only on extension.

## Maintenance findings

- `third_party/smithay-0.7.0/Cargo.toml` is Cargo-normalized and warns not to hand-edit it. Vendor updates should replace the package from one pinned upstream revision and retain `Cargo.toml.orig` provenance.
- Reference sources and vendor sources serve different purposes: Smithay is compiled; `reference/` is comparison material. Documentation should not imply reference code ships in the product.
- Environment-gated GUI and scenario paths are expensive but not dead. Their owners should be named beside gates, with a deterministic offline command, expected platform, and last-known result.
- Large explanatory comments in fixtures should be retained only when they describe observable protocol/test contracts; historical performance narratives belong in audit/history docs rather than executable harness code.

## Recommended removal order

1. Remove the 18-file benchmark island and all direct consumers in one change; run `cargo check -p dd-tests --all-targets` and correctness tests.
2. Re-run the exact manifest and investigate only `VERIFY` candidates with build/runtime tracing.
3. Review reference gitlinks and the MoltenVK slice against active rendering plans; update `reference/LOCK.md` before any pruning.
4. Keep Smithay whole unless adopting a documented minimal-vendor fork with upstream-update automation.
