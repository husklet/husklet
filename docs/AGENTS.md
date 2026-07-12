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
- **`build.rs` and the C engine.** The engine's `#include`d `.c` files are not all tracked by
  `rerun-if-changed`, so after editing engine C, force a rebuild (`cargo clean -p dd-jit-darwin --release`)
  or you ship a stale engine. `make app` already does this.

## 4. The mac-crates post-merge gate (cross-cutting type changes)

Three crates are **not** in the workspace `default-members` and so are **never compiled by a plain
`cargo build`**:

- `dd-display` — the host Wayland renderer (legacy `server.rs` compositor + the Cocoa/Metal present path).
- `dd-compositor` — the Smithay-native compositor (behind `DD_DISPLAY_SMITHAY=1`); links `libxkbcommon`.
- `dd-gpu-wgpu` — the wgpu host GPU executor (behind `DD_GPU_BACKEND=wgpu`).

They are excluded so the headless Linux dev build stays green and offline (Smithay pulls in
`libxkbcommon`; the Cocoa/Metal path is macOS-only). The cost of that exclusion: **a change to a shared
type these crates depend on — e.g. a new field on `dd-display`'s `present::SurfaceBuffer`, or the GPU IR —
compiles clean under `cargo build` yet breaks the un-gated crates.** This has bitten twice.

**After any merge that touches shared types used by the renderer crates, run:**

```
make mac-crates
```

It builds all three crates and runs `dd-compositor` + `dd-gpu-wgpu`'s tests on the macOS toolchain, with
`libxkbcommon` supplied by the nix dev shell (exported as `DD_LIBXKBCOMMON`, wired onto `RUSTFLAGS` and
`DYLD_LIBRARY_PATH`). On a non-macOS host the target no-ops with a note (the crates can't build there).
