# dd — build & test guide

The product is macOS (arm64): the engine is a Mach-O JIT built by the macOS toolchain. Contributors
build and test on a Mac.

## 1. Prerequisites

- **Rust** — the version is pinned by `rust-toolchain.toml`.
- **Nix** — provides the GTK4 GUI build/runtime deps and the packaging tools, plus `libxkbcommon` for the
  Wayland-renderer crates. Enter the dev shell with `nix develop ./nix` (the `make app`/`make dmg`/
  `make mac-crates` targets do this for you).

## 2. Common targets (`make`)

| Target | What it does |
|---|---|
| `make jit` | Build + codesign the guest-arch JIT engines and the workspace crates. |
| `make test` | Run the engine × case matrix (`FILTER=name ENGINE=x86_64` narrow it). |
| `make test-ci` | The `cargo test` matrix path (one test; for CI). |
| `make mac-crates` | **Post-merge gate** for the mac-only Wayland-renderer crates — see §4. |
| `make app` / `make dmg` | Assemble + ad-hoc-sign `dd.app` / build the `.dmg` (needs the nix dev shell). |

`make help`-style descriptions live inline next to each target in the `Makefile`.

## 3. Gotchas

- **Engine resolution.** The daemon finds its JIT engines (`ddjit-*`) via `$DDJIT_DIR` → the path baked
  at build time → `/Applications/dd.app/Contents/Resources`. When running against a fresh build, pin
  `DDJIT_DIR` to that build's out-dir or you silently exercise the stale installed engine.
- **`build.rs` and the C engine.** The build script recursively emits `rerun-if-changed` directives for
  the engine source trees. Packaging still forces a clean `dd-jit-darwin` release rebuild as an additional
  freshness guard; do not infer that ordinary source edits require a manual clean.

## 4. The mac-crates post-merge gate (cross-cutting type changes)

Two renderer crates are **not** in the workspace `default-members` and so are **never compiled by a plain
`cargo build`**:

- `dd-compositor` — the Smithay-native compositor (behind `DD_DISPLAY_SMITHAY=1`); links `libxkbcommon`.
- `dd-gpu-wgpu` — the wgpu host GPU executor (behind `DD_GPU_BACKEND=wgpu`).

`dd-display` is a default member and its platform-neutral core is compiled by a plain workspace build;
its Cocoa/Metal modules remain target-gated. Smithay and wgpu are excluded so the headless Linux dev build
stays green and offline. The cost of that exclusion: **a change to a shared presenter type or the GPU IR
can compile clean under `cargo build` yet break the excluded crates.**

**After any merge that touches shared types used by the renderer crates, run:**

```
make mac-crates
```

It builds all three crates and runs `dd-compositor` + `dd-gpu-wgpu`'s tests on the macOS toolchain, with
`libxkbcommon` supplied by the nix dev shell (exported as `DD_LIBXKBCOMMON`, wired onto `RUSTFLAGS` and
`DYLD_LIBRARY_PATH`). On a non-macOS host the target no-ops with a note (the crates can't build there).
