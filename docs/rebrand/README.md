# dd → husklet rebrand — MAP INDEX (gather-only, NOT executed)

Status: **inventory complete, nothing renamed.** These docs record every `dd`-branded
surface and its proposed target. Execution is a separate, explicit step.

## Decided scheme (from the user)
- **Env vars — ALL → `HL_`**: `DD_*`→`HL_*`, `DDJIT_*`→`HL_*`, and the bare internal control
  flags too (`CRASHDBG`→`HL_CRASHDBG`, `JT`→`HL_JT`, `NOSMC`→`HL_NOSMC`, …).
- **Code symbols — flat**: `dd_*` and `ddjit_*` fns/types/macros/globals → `hl_*`
  (`struct ddjit_config`→`struct hl_config`).
- **Crates — `dd-X`→`husklet-X`** (Rust idents `dd_x`→`husklet_x`).
- **Fresh cutover — no back-compat**: `~/.dd`→`~/.husklet`, `user.dd.*`/`user.ddx.*`→`user.hl.*`,
  `/tmp/dd-*`,`/tmp/ddjit-*`→`/tmp/hl-*`.
- **User-facing brand strings** → `husklet`; short prefixes → `hl`/`HL`.

## The three inventory docs
1. [`dd-jit-inventory.md`](dd-jit-inventory.md) — **dd-jit + dd-jit-darwin** (public API + engine).
   131 env vars · ~40 C fns + ~14 structs · ABI/FFI seam · xattr keys · artifacts.
2. [`dd-cli-daemon-inventory.md`](dd-cli-daemon-inventory.md) — **dd-cli/daemon/client/images**.
   13 env vars · 4 symbols + ~30 crate-refs · 4 pkgs/3 libs/2 bins · the cross-crate setters.
3. [`dd-gpu-frontend-inventory.md`](dd-gpu-frontend-inventory.md) — **dd-gpu/display/term-core/gui/tests + root**.
   ~60 env vars · ~30 symbols · 5 pkgs/4 libs/4 bins · 3 wire magics · 4 `com.dd.*` · Cargo/Makefile/nix/website.

(Env-var counts overlap across docs — many `DD_*` are set in one crate and read in the engine.)

## LOCKSTEP groups — must rename ATOMICALLY or silently break
- **FFI/ABI wire seam**: C `ddjit_spawn` + `struct ddjit_config` (112-byte layout) + `DDJIT_CONFIG_MAGIC`,
  matched in C (`ffi.c`/`ddjit_api.h`) AND Rust (`spawn.rs`/`wire.rs`). Magic mismatch → "bad magic" refusal.
- **GPU/display 3-hop env contract**: `dd-gpu/src/integration.rs` injects `DD_CUDA_*`/`DD_GPU_EXEC`
  → engine forwards via `DD_GUEST_ENV` → guest `cuda_shim.c`/`nvml_shim.c` getenv. Plus socket
  `DD_GPU_EXEC_SOCK`/`/run/user/0/dd-gpu-0`, `DD_DMABUF_MOD_MAGIC 0x6464` (="dd") + `DD_DMABUF_RENDER_BIT`
  mirrored in `dd-display/server.rs` ↔ engine `include/dd_gpu.h`, and mach service `com.dd.display.gpu`
  (`metal.rs` ↔ engine `vfs.c`). Rename across engine + dd-gpu + dd-display + guest shim together.
- **dd-cli → engine env setters**: `DDJIT_DIR`, `DDJIT_CHECKPOINT_DIR`, `DDJIT_RESTORE_DIR` (set in
  dd-cli, read in engine `guest.rs`/`checkpoint.c`).
- **on-disk path contracts**: `/tmp/.ddbr-<netid>` (daemon `ports.rs` ↔ engine `netns.c`),
  `/tmp/.ddnet-<key>` (`ports.rs` ↔ `linux_aarch64.c`); intra-layer `DDOCKERD_SOCK`/`DD_IMAGES`/`DD_STATE`/`DD_VOLUMES`
  set by dd-cli, read by dd-daemon+dd-client (separate bins → half-rename falls back to defaults silently).
- **workspace root, atomic with crate-dir renames**: `Cargo.toml` `members` + `default-members`
  (both lists), all `../dd-*` path deps; `Makefile` `-p dd-*` flags + `$(HOME)/.dd/images` + `/Applications/dd.app`;
  `nix/flake.nix` `ddmac-*` derivations + "dd-app" devshell; `website/` (~875 `\bdd\b` + `dd-*` assets).

## COLLISIONS — flat scheme maps two names to one; need per-case decision
- `DD_SANDBOX` (engine) + `DDJIT_SANDBOX` (tests) → both `HL_SANDBOX`.
- `DD_VOLUMES` **overloaded**: daemon named-volumes *dir* vs engine darwinjail *volume list* — both `HL_VOLUMES`
  (never coexist in one process; user may prefer daemon `HL_VOLUMES_DIR`).
- `DD_DMABUF_*` exists as a Rust const (dd-display) AND a C macro (engine) — same name, two files (intended mirror).
- `dd_root()` (dd-cli) / `dd_home()` (dd-daemon) — same `~/.dd` concept, distinct idents (rename in lockstep).
- Excluded false positives: `DD_PTX` (substring of `VECADD_PTX`), incidental `add`/`padding`/`middleware`.

## OPEN DECISIONS for the user (before execution)
- **Binary names** (user-facing): `ddcli`, `dd-daemon`, `ddockerd`, `dd-term`, `dd-app`, `dd-display` → `hl…`? `husklet…`?
- **Brand for service/ids**: `com.dd.display.gpu` + the 4 `com.dd.*` → `com.husklet.*` or `com.hl.*`?
- **DD_VOLUMES** disambiguation (see collisions).
- **External / persisted refs** (cross-version compat): image ref `huttarichard/ddmac:latest`; archive keys
  `dd-manifest.json`/`dd-image.json` (break `docker save`/`load` across versions); `docker.sock` — KEEP for Docker-API compat.
- **Wire magics** (`DDJIT_CONFIG_MAGIC`, `DD_DMABUF_MOD_MAGIC 0x6464`="dd", 3 total): re-encode to a husklet
  brand value? (cosmetic; only matters that C and Rust agree — bump both together.)

## Proposed EXECUTION order (when authorized — NOT done yet)
1. Env vars (mechanical, per-doc, but the lockstep groups above in one commit each).
2. Code symbols `dd_/ddjit_`→`hl_` (resolve the 2 collisions first).
3. Crate dirs + `Cargo.toml`/`Makefile`/`nix` in one atomic commit; fix all `../dd-*` deps.
4. On-disk/paths/service ids/xattrs (fresh cutover) + wire magics (C+Rust together).
5. Website + assets + brand strings.
6. Gate green after each phase (engine phases → `make test` 1636/0, 3 engines).
