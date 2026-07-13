# dd → husklet rebrand inventory: GPU / display / terminal / GUI / tests / workspace-ROOT

READ-ONLY map, produced 2026-07-07. Step 3 of the `dd`→`husklet` rebrand. Ground truth for a later,
mechanical rename. **Scope: `dd-gpu/`, `dd-display/`, `dd-term-core/`, `dd-gui/`, `dd-tests/`, and
the workspace ROOT (`Cargo.toml`, `Makefile`, `nix/`, `website/`, `assets/`, top-level scripts).**
The engine crates `dd-jit/` + `dd-jit-darwin/` are covered by the companion doc
[`dd-jit-inventory.md`](dd-jit-inventory.md); the remaining crates `dd-cli`, `dd-daemon`,
`dd-client`, `dd-images` are **NOT** covered here (a later pass must do them). Where a var/symbol/name
in this scope crosses into the engine or into `dd-cli`/`dd-daemon`, it is flagged in §Risk notes.

Nothing was renamed. All `file:line` refs are real. Word-boundary care was applied: `ADDRESS`,
`HIDDEN`, `MIDDLE`, `ADD_AND_FETCH`, `ADD_SEALS`, `ADDR_NO_RANDOMIZE` false-positives were excluded,
and `DD_PTX` proved to be a substring of `VECADD_PTX` (a PTX kernel constant, **not** a brand token).

---

## Summary counts

| Category | Count |
|---|---|
| Distinct branded env vars in scope (`DD_*` / `DDJIT_*` / `DDOCKERD_*`) | **~60** (see §1) |
| — GPU/display cross-process class (`DD_CUDA_*`, `DD_GPU_*`, `DD_DISPLAY_*`, `DD_DMABUF_*`, `DD_IR_DUMP`) | 13 |
| — daemon/CLI/harness class (`DD_IMAGES`, `DD_STATE`, `DD_DAEMON*`, `DD_CLI_BIN`, `DDOCKERD_SOCK`, …) | 12 |
| — terminal/GUI class (`DD_TERM_*`, `DD_SHOT*`, `DD_WORKSPACE`) | 20 |
| — packaging/signing/notary class (`DD_VERSION`, `DD_SIGN_*`, `DD_NOTARY_*`, `DD_PACK`, bundle nix-path vars) | 15 |
| — engine-tuning refs forwarded from tests (`DDJIT_*`) | 6 distinct |
| Branded symbols (`dd_*` fns/types, CSS `@dd_*`, C macros `DD_*`, magics) | ~30 (see §2) |
| Cargo package / lib / bin targets branded `dd` | 5 pkgs, 4 libs, 4 bins (see §3a) |
| Brand-encoded on-wire magics (`0x6464`, `0xDD6B_0001`, `"DDF2"`) | 3 |
| mach / bundle ids (`com.dd.*`) | 4 |
| On-disk paths (`~/.dd`, `/tmp/dd-*`, `/run/user/0/dd-gpu-0`, sockets) | see §3c |
| Root touchpoints (`Cargo.toml` members, `Makefile`, `nix/`, `website/`, `assets/`) | see §3e |

### Rename-mapping proposal (legend — CONFIRM before executing)

| From | To (proposed) | Note |
|---|---|---|
| env `DD_*` | `HL_*` | all env → `HL_` per decided scheme |
| env `DDJIT_*` (test refs) | `HL_*` | must match the engine doc's decision (JIT infix vs collapse) |
| env `DDOCKERD_SOCK` | `HL…_SOCK` | ties to the `ddockerd` daemon-binary rename — **FLAG, cross-crate** |
| crate `dd-gpu` | `husklet-gpu` | lib `dd_gpu`→`husklet_gpu` |
| crate `dd-display` | `husklet-display` | lib `dd_display`→`husklet_display`; **bin `dd-display`** → FLAG |
| crate `dd-term-core` | `husklet-term-core` | lib `dd_term_core`→`husklet_term_core` |
| crate `dd-gui` | `husklet-gui` | **bins `dd-app`, `dd-term`** → FLAG (user-facing) |
| crate `dd-tests` | `husklet-tests` | lib `dd_tests`; dep-alias `ddjit` (→`husklet-jit`) |
| C fn `dd_mach_server_start`/`dd_mach_recv`, type `dd_gpu_msg_t` | `hl_*` | **FFI, C↔Rust lockstep** (dd-display) |
| C macro `DD_DEVICE_HANDLE`/`DD_ET_SLOTS`/`DD_ET_HEADER`/`DD_CUDA_MIN_H`/`DD_NVML_MIN_H` | `HL_*` | internal nvml/cuda shim macros |
| GTK CSS tokens `@dd_accent` … + classes `.dd-seg`,`.dd-active`,`.dd-batch` | `@hl_*` / `.hl-*` | dd-gui theme (define+use in one file) |
| magic `0x6464` (`DD_DMABUF_MOD_MAGIC`), `0xDD6B_0001` (`KERNEL_MAGIC`), `"DDF2"` (fsrv magic) | new magic | brand-encoded on the wire — must match engine peer |
| `~/.dd` | `~/.husklet` | fresh cutover (incl. `workspaces.conf`, `run/docker.sock`, `nvml/`, `gui/`) |
| `/tmp/dd-*` | `/tmp/hl-*` | display scratch + test scratch |
| `/run/user/0/dd-gpu-0`, `dd-gpu.sock` | `hl-gpu-0` / `hl-gpu.sock` | GPU exec socket — **FLAG, matches guest-shim + display** |
| mach `com.dd.display.gpu` | `com.husklet.display.gpu` (or `com.hl.` — FLAG) | **cross-process with engine `vfs.c:3466`** |
| bundle ids `com.dd.app`/`com.dd.term`/`com.dd.daemon` | `com.husklet.*` | LaunchAgent + Info.plist |
| brand string `"dd Metal (CUDA-sim) Device"`, `"Tesla dd-Metal 4C"` | `"husklet …"` | shows in guest `nvidia-smi` |
| website/`assets` `dd`-brand copy + `dd-*.png/gif`/`logo`/`hello-dd` | `husklet` | bulk brand rebrand |

Ambiguities for the user: (a) do the `dd-display`/`dd-app`/`dd-term` **binary** names rename (they are
what a user types / sees in the Dock)? (b) does `DDOCKERD_SOCK` follow the `ddockerd` binary rename or
collapse to a generic `HL_DAEMON_SOCK`? (c) mach/bundle prefix `com.husklet.` vs `com.hl.`; (d) config
filenames `term.conf`/`term-defaults.conf`/`workspaces.conf` are **not** themselves `dd`-branded —
only their parent dir `~/.dd` is, so they likely KEEP their names.

---

## §1. ENVIRONMENT VARIABLES

Read = `env::var`/`var_os` (Rust), `getenv` (C shim), `$VAR`/`${VAR}` (shell). Set = `env::set_var` /
`.env(` (Rust Command) / `export` (shell) / injected into guest env (`req.env.push`).

### 1a. GPU / display CROSS-PROCESS class — THE PRIORITY (shared engine ↔ dd-gpu launcher ↔ guest .so shim ↔ dd-display)

These form a cross-process contract. `dd-gpu/src/integration.rs` (host launcher) **injects** them into
the guest env; the engine passes them through (`DD_GUEST_ENV`); the guest-side CUDA/NVML `.so` shims
**getenv** them at runtime; `dd-display` reads the socket/debug ones.

| Name | Class | Read sites | Set / inject sites | Purpose | HL target |
|---|---|---|---|---|---|
| `DD_CUDA_NAME` | guest-shim | cuda/cuda_shim.c:46, nvml/nvml_shim.c:46, nvml/test_nvml.c:62 | dd-gpu/src/integration.rs:173,258,281; cuda/build.sh:36, nvml/build.sh:34 | reported GPU device name | `HL_CUDA_NAME` |
| `DD_CUDA_CC` | guest-shim | cuda_shim.c:48, nvml_shim.c:48, test_nvml.c:63 | integration.rs:174,259,281; build.sh (both) | compute capability "maj.min" | `HL_CUDA_CC` |
| `DD_CUDA_VRAM` | guest-shim | cuda_shim.c:50, nvml_shim.c:53, test_nvml.c:64 | integration.rs:175,260,281; build.sh (both) | reported VRAM (MB) | `HL_CUDA_VRAM` |
| `DD_CUDA_DRIVER` | guest-shim | nvml_shim.c:60 | (launcher; default in shim) | reported driver version string | `HL_CUDA_DRIVER` |
| `DD_CUDA_NVML` | guest-shim | nvml_shim.c:67 | — | reported NVML version | `HL_CUDA_NVML` |
| `DD_CUDA_DRIVER_CUDA` | guest-shim | nvml_shim.c:72 | — | reported CUDA driver version | `HL_CUDA_DRIVER_CUDA` |
| `DD_GPU_EXEC` | guest-env | (guest GL/CUDA shim, out of scope) | dd-gpu/src/integration.rs:125,230 | guest-visible GPU exec socket path `/run/user/0/dd-gpu-0` | `HL_GPU_EXEC` |
| `DD_GPU_EXEC_SOCK` | host | dd-display/src/main.rs:291 | (dd-cli/launcher, out of scope) | override for the dd-gpu IR executor socket | `HL_GPU_EXEC_SOCK` |
| `DD_DISPLAY_DUMP` | host-debug | dd-display/src/lib.rs:143 | — | dir for debug PNG dumps (`/tmp/dd-display-selftest`) | `HL_DISPLAY_DUMP` |
| `DD_DISPLAY_DEBUG` | host-debug | dd-display/src/server.rs:19 | — | opt-in wire trace | `HL_DISPLAY_DEBUG` |
| `DD_DISPLAY_DMABUF` | host | dd-display/src/server.rs:225 | — | advertise zwp_linux_dmabuf_v1 (GPU rung 2) | `HL_DISPLAY_DMABUF` |
| `DD_IR_DUMP` | host-debug | dd-display/src/metal_backend.rs:258 (doc'd capture var) | — | dump forwarded GPU IR for one frame | `HL_IR_DUMP` |
| `DD_GPU_IOSURFACE` | (engine-side) | — (not read in this scope; set by engine `ddjit_configfd.c:92`, see engine doc) | — | opt-in host-IOSurface GPU path | `HL_GPU_IOSURFACE` |

> The `DD_CUDA_NAME` default value `"dd Metal (CUDA-sim) Device"` and `nvml/build.sh`'s
> `"Tesla dd-Metal 4C"` are **user-facing brand strings** (surface in guest `nvidia-smi`) — see §3d.

### 1b. Daemon / CLI / test-harness class (cross-crate with `dd-cli` / `dd-daemon` / `ddockerd`)

| Name | Read sites | Set sites | Purpose | HL target |
|---|---|---|---|---|
| `DD_IMAGES` | dd-tests provision/mod.rs:35, bin/scenarios.rs:126; Makefile (`DD_IMAGES`), many `dd-tests/scenarios/*.sh`, tools/memwatch.sh:20 | Makefile:`DD_IMAGES ?=`, scenario shells | image store dir | `HL_IMAGES` |
| `DD_DAEMON` | dd-tests/src/bin/scenarios.rs:129 | — | daemon binary/path override for scenarios | `HL_DAEMON` |
| `DD_DAEMON_BIN` | dd-gui/src/daemon.rs:59 | — | GUI's daemon-binary override | `HL_DAEMON_BIN` |
| `DD_DAEMON_LOG` | dd-gui/src/snapshot.rs:124 | — | daemon log path for the crash snapshot | `HL_DAEMON_LOG` |
| `DD_CLI_BIN` | dd-gui/src/install.rs:26, ui/views/workspaces.rs:192 | — | `ddcli` binary override | `HL_CLI_BIN` |
| `DD_STATE` | (daemon; out of scope) | dd-tests/src/scenario/daemon.rs:101 + all `dd-tests/scenarios/*.sh`, tools/memwatch.sh:20 | daemon state.json path | `HL_STATE` |
| `DD_VOLUMES` | (daemon; out of scope) | dd-tests/src/scenario/daemon.rs:101 + `dd-tests/scenarios/*.sh` | volumes root | `HL_VOLUMES` |
| `DDOCKERD_SOCK` | dd-gui/src/ui/components/terminal.rs:231; all `dd-tests/scenarios/*.sh`, tools/memwatch.sh:20 | scenario shells | `ddockerd` daemon socket — **FLAG: follows `ddockerd` rename** | `HL_…_SOCK` |
| `DD_DEBUG` | dd-tests/tests/overlay.rs:156, src/harness/run/mod.rs:191 | — | harness debug gate | `HL_DEBUG` |
| `DD_SCEN_JOBS` | dd-tests/src/bin/scenarios.rs:176 | — | scenario parallelism | `HL_SCEN_JOBS` |
| `DD_SCEN_PROFILE` | dd-tests/src/scenario/drive/mod.rs:44 | — | scenario profile selector | `HL_SCEN_PROFILE` |
| `DD_ENV` | (guest `printenv` fixture) | dd-tests/src/scenarios/process/mod.rs:13 (`-e DD_ENV=hello123`) | arbitrary test fixture var — low value | `HL_ENV` |

### 1c. Terminal / GUI class (`dd-gui` bins `dd-term`+`dd-app`, `dd-term-core`)

All in `dd-gui/src/bin/term.rs` unless noted. Read-only unless a set site is given.

| Name | Sites | Purpose | HL target |
|---|---|---|---|
| `DD_TERM_VIEW` | term.rs:352,3615 | initial view selector | `HL_TERM_VIEW` |
| `DD_TERM_WS` | term.rs:355 | target workspace | `HL_TERM_WS` |
| `DD_TERM_SETTINGS_PANE` | term.rs:812 | open settings pane | `HL_TERM_SETTINGS_PANE` |
| `DD_TERM_NEWWS_PANE` | term.rs:1249 | open new-workspace pane | `HL_TERM_NEWWS_PANE` |
| `DD_TERM_TABS` | term.rs:2119 | tabs config | `HL_TERM_TABS` |
| `DD_TERM_SPLIT` | term.rs:2125 | split layout | `HL_TERM_SPLIT` |
| `DD_TERM_DASH` | term.rs:2133 | dashboard toggle | `HL_TERM_DASH` |
| `DD_TERM_DASHPANE` | term.rs:3009 | dashboard pane | `HL_TERM_DASHPANE` |
| `DD_TERM_CMD` | term.rs:2777 | command to run | `HL_TERM_CMD` |
| `DD_TERM_DEBUG_LOG` | term.rs:2778 | debug log path | `HL_TERM_DEBUG_LOG` |
| `DD_TERM_TYPE` | term.rs:2858 | terminal type | `HL_TERM_TYPE` |
| `DD_TERM_SHOT` | term.rs:3614 | screenshot mode | `HL_TERM_SHOT` |
| `DD_TERM_SHOT_MS` | term.rs:3618 | screenshot delay | `HL_TERM_SHOT_MS` |
| `DD_SHOT` | dd-gui/src/main.rs:201 | GUI screenshot mode | `HL_SHOT` |
| `DD_SHOT_VIEW` | dd-gui/src/main.rs:149 | screenshot view | `HL_SHOT_VIEW` |
| `DD_SHOT_DELAY_MS` | dd-gui/src/main.rs:204 | screenshot delay | `HL_SHOT_DELAY_MS` |
| `DD_WORKSPACE` | **set**: dd-term-core/src/workspace.rs:548 (guest env), used workspace.rs:737 (test) | workspace name injected into each pane shell | `HL_WORKSPACE` |

### 1d. Packaging / signing / notary class (ROOT `Makefile` + `dd-gui/package/` + `dd-gui/mac/` + `nix/`)

| Name | Sites | Purpose | HL target |
|---|---|---|---|
| `DD_VERSION` | dd-gui/build.rs:9 (read → baked), Makefile (`app:` sets `DD_VERSION=$(VERSION)`), dd-gui/package/bundle.sh | app version baked into `dd-app` | `HL_VERSION` |
| `DD_SIGN_ID` / `DD_SIGN_KEYCHAIN` / `DD_SIGN_KEYCHAIN_PW` / `DD_SIGN_P12_BASE64` / `DD_SIGN_P12_PW` | dd-gui/package/bundle.sh + package/signing/ | Developer-ID codesign inputs | `HL_SIGN_*` |
| `DD_NOTARY_APPLE_ID` / `DD_NOTARY_PROFILE` / `DD_NOTARY_PW` / `DD_NOTARY_TEAM_ID` | dd-gui/package (notarize) | notarization inputs | `HL_NOTARY_*` |
| `DD_PACK` | dd-gui/mac/mac-userland.sh:43,63 | pack nix closure into rootfs | `HL_PACK` |
| `DD_MAC_IMAGE` | dd-gui/mac/*.sh | macOS dev-container image selector | `HL_MAC_IMAGE` |
| `DD_GTK4` / `DD_LIBRSVG` / `DD_GDK_PIXBUF` / `DD_GSETTINGS_SCHEMAS` / `DD_HICOLOR_ICONS` / `DD_ADWAITA_ICONS` | dd-gui/package/bundle.sh (fed from `nix/`) | nix store paths for GTK bundle assembly | `HL_*` (internal bundle plumbing) |
| `DD_DOCKER` | Makefile (`DD_DOCKER ?= docker --host …`) | docker-CLI-against-dd-daemon helper var (Make var, not runtime env) | `HL_DOCKER` |

### 1e. Engine-tuning env referenced FROM the tests (`DDJIT_*` — must match engine-doc decision)

The harness sets these to exercise the engine; the readers live in `dd-jit-darwin/` (engine doc §1b).

| Name | Set/ref sites (this scope) | Purpose | HL target |
|---|---|---|---|
| `DDJIT_DIR` | dd-tests/src/bin/bench.rs:121 (read), :140 (set) | locate engine artifacts for the bench | `HL_DIR` |
| `DDJIT_PCACHE` | dd-tests/tests/pcache.rs:134,348,510,574; forkserver.rs:295; src/cases/regress.rs:107, ext/pcachex.rs:26 | enable persistent code cache | `HL_PCACHE` |
| `DDJIT_PCACHE_DIR` | dd-tests/tests/pcache.rs:135,349,511,575; regress.rs:107 | pcache dir | `HL_PCACHE_DIR` |
| `DDJIT_NOPCACHE` | dd-tests/tests/pcache.rs:282; ext/pcachex.rs:15 | kill switch | `HL_NOPCACHE` |
| `DDJIT_NOFASTSYS` | dd-tests/src/cases/syscall.rs:125,133,142 | force slow syscall path | `HL_NOFASTSYS` |
| `DDJIT_UNTRUSTED` / `DDJIT_SANDBOX` | dd-tests/src/cases/container.rs:53,55 | sentry/untrusted gates | `HL_UNTRUSTED` / (sandbox: see engine-doc collision note) |

Non-branded knobs the harness also forwards (KEEP): `COLDPROF`, `CRASHDBG` (run/mod.rs:206), `PERF`,
`PERF_N` (src/main.rs:119-120), `BENCH_N`, `BENCH_K` (bench.rs:145,149). Standard toolchain env in
build.rs files (KEEP, not branded): `CARGO_CFG_TARGET_OS`, `OUT_DIR`, `CC`, `AR` (dd-display/build.rs).

---

## §2. BRANDED SYMBOLS (functions / types / macros / CSS tokens / magics)

### 2a. FFI — C ↔ Rust lockstep (dd-display mach/GPU bridge)

| Symbol | Kind | Definition | Mirror / user | HL target |
|---|---|---|---|---|
| `dd_mach_server_start` | C fn | dd-display/src/mach_bridge.c:30 | Rust `extern "C"` dd-display/src/metal.rs:67, called :88 | `hl_mach_server_start` |
| `dd_mach_recv` | C fn | mach_bridge.c:42 | metal.rs:68, called :97 | `hl_mach_recv` |
| `dd_gpu_msg_t` | C typedef | mach_bridge.c:24 (used :44) | — | `hl_gpu_msg_t` |
| link lib / obj `dd_mach_bridge` | build artifact | dd-display/build.rs:16 (`dd_mach_bridge.o`), :31 (`rustc-link-lib=static=dd_mach_bridge`) | Rust link | `hl_mach_bridge` |

> mach service string `"com.dd.display.gpu"` (metal.rs:73) is the bootstrap name — see §3c; it must
> match the engine's `os/linux/container/vfs.c:3466` (cross-process, see Risk notes).

### 2b. CUDA / NVML shim C symbols & macros (`dd-gpu/cuda/`, `dd-gpu/nvml/`)

| Symbol | Kind | Site | HL target |
|---|---|---|---|
| `DD_DEVICE_HANDLE` | C macro (fake nvmlDevice_t) | nvml/nvml_shim.c:43 (used :79,138,147,154) | `HL_DEVICE_HANDLE` |
| `DD_ET_SLOTS` / `DD_ET_HEADER` | C macros (export-table sizing) | nvml_shim.c:401-402 (used :403,411,412) | `HL_ET_SLOTS` / `HL_ET_HEADER` |
| `dd_et_notsup` | C fn (export-table stub) | nvml_shim.c:400 (used :412) | `hl_et_notsup` |
| `DD_CUDA_MIN_H` | include guard | cuda/cuda_min.h:14,15,317 | `HL_CUDA_MIN_H` |
| `DD_NVML_MIN_H` | include guard | nvml/nvml_min.h (guard) | `HL_NVML_MIN_H` |

### 2c. dd-gpu Rust IR / integration symbols

The public IR/wire/backend types are mostly **un-branded** (`GpuBackend`, `KernelDescriptor`,
`GpuError`, `DeviceProvider`, `DeviceRequest`) — no `dd`/`Dd` prefix, so no symbol rename needed.
The only brand tokens are the env strings in §1a and:

| Symbol | Kind | Site | HL target |
|---|---|---|---|
| `KERNEL_MAGIC = 0xDD6B_0001` | Rust const (kernel-blob magic) | dd-gpu/src/ptx.rs:205 | new magic — the leading `DD` half-encodes the brand; pick a fresh value (peer: the guest shim that reads the blob) |

### 2d. dd-gui GTK theme CSS tokens (dd-gui/src/ui/theme.rs — define + use in this one file)

`@define-color` tokens: `@dd_accent` (:15), `@dd_accent_hi`, `@dd_green` (:17), `@dd_red` (:18),
`@dd_amber` (:19), `@dd_line` (:20), `@dd_line_soft`, `@dd_fill` (:22), `@dd_fill_hi`, `@dd_text` (:25),
`@dd_text_dim`, `@dd_text_faint`. CSS classes: `.dd-seg` (:87,93), `.dd-active` (:88), `.dd-batch`
(:79). All self-contained to theme.rs → `@hl_*` / `.hl-*`.

### 2e. dd-tests bench output column identifiers

`dd-tests/src/bin/bench.rs:288,316` — CSV/JSON columns `dd_arm64_x`, `dd_x86_x`, `dd_arm64_ms`,
`dd_x86_ms` (in `target/dd-tests/bench.{csv,json}`). User-facing report labels → `hl_arm64_*` etc.

### 2f. Brand-encoded on-wire MAGICS (must match the non-scope peer)

| Magic | Value | Site | Peer | Note |
|---|---|---|---|---|
| `DD_DMABUF_MOD_MAGIC` | `0x6464` (= ASCII `"dd"`) | dd-display/src/server.rs:38 (used :297,591) | engine `include/dd_gpu.h` (same name, engine doc §2d) | dmabuf modifier magic; **doubly** encodes the brand |
| `DD_DMABUF_RENDER_BIT` | `0x1_0000` | dd-display/src/server.rs:39 (used :593) | engine `include/dd_gpu.h` | render-path bit |
| `"DDF2"` / `FSRV_MAGIC 0x32464444` | forkserver framing magic | dd-tests/tools/fclient.c:11,30 | engine forkserver | test client for `ddockerd`/forkserver |

---

## §3. NAMES: crates, targets, paths, ids, brand strings, ROOT touchpoints

### 3a. Cargo package / lib / bin targets

| Item | Location | Value | HL target |
|---|---|---|---|
| package | dd-gpu/Cargo.toml:14 | `dd-gpu` | `husklet-gpu` |
| lib | dd-gpu/Cargo.toml:21 | `dd_gpu` | `husklet_gpu` |
| dep (optional) | dd-gpu/Cargo.toml (`dd-jit = { path = "../dd-jit", optional }`, `dep:dd-jit`) | `dd-jit` | update to `husklet-jit` |
| package | dd-display/Cargo.toml:2 | `dd-display` | `husklet-display` |
| **bin** | dd-display/Cargo.toml:10 | `dd-display` | FLAG (invoked binary) |
| lib | dd-display/Cargo.toml:14 | `dd_display` | `husklet_display` |
| deps | dd-display/Cargo.toml | `dd-term-core`, `dd-gpu` (path deps) | update paths+names |
| package | dd-term-core/Cargo.toml:2 | `dd-term-core` | `husklet-term-core` |
| lib | dd-term-core/Cargo.toml:9 | `dd_term_core` | `husklet_term_core` |
| package | dd-gui/Cargo.toml:2 | `dd-gui` | `husklet-gui` |
| **bin** | dd-gui/Cargo.toml:10 | `dd-app` | FLAG (Dock/app name) |
| **bin** | dd-gui/Cargo.toml:16 | `dd-term` | FLAG (terminal binary) |
| deps | dd-gui/Cargo.toml | `dd-client`, `dd-term-core` (path) | update paths+names |
| package | dd-tests/Cargo.toml:2 | `dd-tests` | `husklet-tests` |
| bins | dd-tests/Cargo.toml:11,16 | `dd-tests`, `scenarios` (+ auto `bench`) | `husklet-tests` / keep `scenarios`,`bench` |
| lib | dd-tests/Cargo.toml:20 | `dd_tests` | `husklet_tests` |
| dep **alias** | dd-tests/Cargo.toml | `ddjit = { path = "../dd-jit", package = "dd-jit" }` | alias `ddjit`→ e.g. `hljit`; used as `use ddjit::…` in code |
| descriptions | all 5 Cargo.toml `description=` | prose "dd …" / "dd-app …" / "dd-term …" | rebrand prose |

Rust crate-ident references (update alongside the lib renames): `dd_gpu` (25×), `dd_display` (23×),
`dd_term_core` (27×), `dd_tests` (5×), `dd_client` (14×, dep of dd-gui), `ddjit`/`dd_jit`. E.g.
dd-display uses `dd_term_core::…` + `dd_gpu::…`; dd-gui uses `dd_term_core::…` + `dd_client::…`.

### 3b. Build artifacts

| Artifact | Site | HL target |
|---|---|---|
| `dd_mach_bridge.o` / static lib `dd_mach_bridge` | dd-display/build.rs:16,31 | `hl_mach_bridge` |
| `dd.app` bundle | dd-gui/package/bundle.sh, make-dmg.sh, Makefile `install:` (`/Applications/dd.app`) | `husklet.app` |
| `dist/dd-<ver>-<arch>.dmg` | dd-gui/package/make-dmg.sh, Makefile `dmg:` | `husklet-<ver>-<arch>.dmg` |
| cuda/nvml shim `.so` (built into `~/.dd/c`, `~/.dd/n`) | dd-gpu/cuda/build.sh, nvml/build.sh | path → `~/.husklet/…` |

### 3c. On-disk paths, sockets, mach / bundle ids (fresh-cutover + cross-process)

| String | Sites | Kind | HL target |
|---|---|---|---|
| `~/.dd` (+ `.dd/`) | dd-gpu/src/integration.rs:9; dd-gpu/cuda/build.sh:7,41 (`.dd/c`), nvml/build.sh:7,39 (`.dd/n`); dd-gui/src/{main.rs:77,170,389, daemon.rs:12, install.rs, snapshot.rs:127, ui/**, bin/term.rs (many)}; dd-term-core/src/workspace.rs:320 (`~/.dd/workspaces.conf`); Makefile (`DD_IMAGES ?= $(HOME)/.dd/images`, `$(HOME)/.dd/run/docker.sock`) | home state dir | `~/.husklet` |
| `~/.dd/run/docker.sock` | dd-gui/src/{daemon.rs:36, ui/components/terminal.rs:229,233}; Makefile `DD_DOCKER` | daemon socket path | dir→`.husklet`; socket name `docker.sock` KEEP (Docker-API compat) |
| config `term.conf` / `term-defaults.conf` | dd-gui/src/bin/term.rs (184,202,506,508,514,520,553,627,635,658,831,857,893) | files under `~/.dd` | KEEP filename (not `dd`-branded); only dir moves |
| config `workspaces.conf` | dd-term-core/src/workspace.rs:320 | file under `~/.dd` | KEEP filename; dir moves |
| `/run/user/0/dd-gpu-0` | dd-gpu/src/integration.rs:47,124,125,212,222,230 | guest GPU exec socket dir | `/run/user/0/hl-gpu-0` (peer: guest shim + display) |
| `dd-gpu.sock` | dd-display/src/main.rs:287,296; dd-gpu/src/integration.rs:212,222 | GPU IR executor socket file | `hl-gpu.sock` |
| `/tmp/dd-display*` (`-selftest`,`-input`,`-metal`,`-shader`,`-shim-ir`,`-texture`,`-indexed`,`-replay`,`-render`,`-iosurface`,`-cocoa`,`-pattern`,`-metal-`,`-cocoa-`,`-selftest-`) | dd-display/src/{lib.rs:143,255, main.rs:28-185, metal.rs:518, present_cocoa.rs:269, selftest.rs:145}, examples/render_pattern.rs:17 | display scratch/dump dirs | `/tmp/hl-display*` |
| `/tmp/dd-inotify`, `/tmp/dd_w_probe`, `/dd_ro_probe` | dd-tests/src/scenarios/weird/native.rs:124; cases/ext/isolation.rs:120-121; guests/ext_iso/rofs.c | test-fixture paths | `/tmp/hl-*` (test-local) |
| `/tmp/ddjit-pcache-*` | dd-tests/src/cases/{regress.rs:107, ext/pcachex.rs:27} | pcache dirs (engine default family) | `/tmp/hl-pcache-*` (match engine doc §3c) |
| mach `com.dd.display.gpu` | dd-display/src/metal.rs:73 | **cross-process** bootstrap name (engine `vfs.c:3466`) | `com.husklet.display.gpu` (or `com.hl.` — FLAG) |
| bundle `com.dd.app` | dd-gui/package/Info.plist.in:6, dd-gui/src/main.rs:521 | CFBundleIdentifier | `com.husklet.app` |
| LaunchAgent `com.dd.daemon` | dd-gui/src/daemon.rs:53 | daemon agent label | `com.husklet.daemon` |
| mach/agent `com.dd.term` | dd-gui/src/bin/term.rs:27 | terminal app id | `com.husklet.term` |

> The SysV/`/di…` prefix is NOT in this scope. The `docker.sock` filename is Docker-API compat, not
> brand — recommend KEEP even under the `.husklet` dir.

### 3d. User-facing brand strings

- `"dd Metal (CUDA-sim) Device"` — default GPU name shown by guest `nvidia-smi`/CUDA: dd-gpu
  cuda/cuda_shim.c:23, nvml/nvml_shim.c:11, integration.rs:258, test_nvml.c:62, **and**
  dd-term-core/src/workspace.rs:167,179. → `"husklet Metal (CUDA-sim) Device"`.
- `"Tesla dd-Metal 4C"` — nvml/build.sh:34 default `DD_CUDA_NAME`. → `"Tesla husklet-Metal 4C"`.
- bin names `dd-app`, `dd-term`, `dd-display` (Dock/CLI-visible) and `dd.app` bundle — see §3a/§3b.
- `hello-dd` — the bundled sample image name: dd-gui/package/bundle.sh:51, src/main.rs:168,
  ui/views/home.rs:82,96, **and** on disk `assets/images/hello-dd/`. → `hello-husklet` (rename dir + refs).
- `DD_WORKSPACE` guest env / `echo hello-$DD_WORKSPACE` — dd-term-core/src/workspace.rs:548,737.
- heredoc marker `DDEOF` (dd-tests/src/scenario/drive/mod.rs:135-143, scenarios/toolchains/mod.rs:10)
  — cosmetic, arbitrary delimiter; optional rename.

### 3e. ROOT workspace touchpoints (MUST change atomically with the crate-dir renames)

| File | Item | Detail |
|---|---|---|
| `Cargo.toml` (root) | `members = [ … ]` + `default-members = [ … ]` | list every crate dir `dd-jit, dd-jit-darwin, dd-images, dd-daemon, dd-client, dd-tests, dd-cli, dd-gui, dd-term-core, dd-gpu, dd-display` — **renaming a crate dir requires editing BOTH lists here in the same commit**; header comment "dd — VM-less Linux container runtime" + inline comments name `dd-gui` |
| `Makefile` | targets/vars | `.PHONY` names KEEP; but `DD_IMAGES`, `DD_DOCKER`, `DD_VERSION`, `DDMAC_REPO ?= huttarichard/ddmac`, `DDMAC_TOKEN`, `DD_IMAGES ?= $(HOME)/.dd/images`; `-p dd-gui`/`-p dd-daemon`/`-p dd-cli`/`-p dd-tests` package refs; `dd-gui/package/*.sh` paths; `/Applications/dd.app`; `dd-tests/scenarios/*.sh` paths; comment "dd workspace." |
| `nix/flake.nix` | derivation names | description "dd-app GTK4 dev shell" (:8); `mkEnv "ddmac-base"` / `"ddmac-dev"` (:35-37) dev-container image names |
| `website/index.html` | brand copy | `<title>dd — run Linux containers on macOS, with no VM</title>` (:6), `og:title` (:8), nav/hero brand "dd" (:35,52), comparison tables "dd — userspace kernel (JIT)" (:122), "dd" column (:158) — **~875 `\bdd\b` word occurrences** across index.html + roadmap.html + blog/ (bulk rebrand) |
| `website/roadmap.html` | brand copy | same "dd" brand throughout |
| `website/assets/` | asset filenames | `dd-docker-poster.png`, `dd-docker.gif`, `dd-inside-poster.png`, `dd-inside.gif`, `dd-run-poster.png`, `dd-run.gif`, `logo.png`, `logo@2x.png`, `favicon*.png`, `apple-touch-icon.png` (rename `dd-*` assets + the `<img src>`/`og:image` refs) |
| `assets/` (root) | `logo.png`, `images/hello-dd/` | app logo + sample image rootfs dir `hello-dd` |
| `README.md` (root) | brand prose | "dd" throughout (bulk) — not enumerated line-by-line |

---

## Risk notes (lockstep / careful)

### GPU/display cross-process env contract shared with the engine + launcher (THE priority)
The `DD_CUDA_*` / `DD_GPU_EXEC` family is a **3-hop** contract: `dd-gpu/src/integration.rs` (host
launcher) pushes them into `DeviceRequest.env` → the engine forwards them into the guest via
`DD_GUEST_ENV` (engine doc §1a) → the guest-side CUDA/NVML `.so` shims (`cuda_shim.c`/`nvml_shim.c`,
built into `~/.dd/c`,`~/.dd/n`) `getenv` them. A rename here must land **simultaneously** in: (a) the
`integration.rs` setters, (b) the shim `getenv`s, (c) the `build.sh` defaults, (d) any `dd-cli`/
`dd-daemon` launch code that also sets them (out of scope — must coordinate). `DD_GPU_EXEC_SOCK` /
`dd-gpu.sock` / `/run/user/0/dd-gpu-0` are the socket half of the same contract, shared between
`dd-gpu`, `dd-display`, and the guest shim.

### Cross-process magics that must stay in sync with the engine/peer
- `DD_DMABUF_MOD_MAGIC 0x6464` + `DD_DMABUF_RENDER_BIT 0x1_0000` (dd-display/src/server.rs:38-39)
  mirror the engine's `include/dd_gpu.h` constants of the same name (engine doc §2d). `0x6464` is
  literally ASCII `"dd"` — if the brand-encoding is "fixed", the engine peer must change to match, or
  dmabuf negotiation silently mismatches.
- mach service `com.dd.display.gpu` (metal.rs:73) must equal the engine's registered name
  (`os/linux/container/vfs.c:3466`) — a one-sided rename breaks IOSurface hand-off.
- `"DDF2"`/`0x32464444` forkserver magic (dd-tests/tools/fclient.c) mirrors the engine forkserver.

### Cross-crate names set/consumed OUTSIDE this scope (coordinate with the dd-cli/daemon pass)
- `DDOCKERD_SOCK` is the `ddockerd` daemon socket — it names the (out-of-scope) `ddockerd` binary;
  its rename must be decided with the `dd-daemon`/`dd-cli` pass. Set by every `dd-tests/scenarios/*.sh`
  and read by `dd-gui` terminal + tests.
- `DD_STATE` / `DD_VOLUMES` are **read by `dd-daemon`** (out of scope) but **set** here (tests) — rename
  must be simultaneous with the daemon.
- `DD_IMAGES` is read here (tests) + in `dd-daemon` + set by the root `Makefile`.
- `DD_CLI_BIN` / `DD_DAEMON_BIN` point at the `ddcli`/`ddockerd` binaries (out-of-scope rename targets).

### Root-atomic (the pieces that must change WITH the crate-dir renames)
- Root `Cargo.toml` `members` **and** `default-members` (note `dd-gui` is only in `members`, excluded
  from `default-members`) — both lists reference every `dd-*` dir. Renaming a dir without editing both
  breaks the workspace.
- `Makefile` `-p dd-gui`/`-p dd-daemon`/`-p dd-cli`/`-p dd-tests` package flags, the `dd-gui/package/`
  + `dd-gui/mac/` + `dd-tests/scenarios/` script paths, and `/Applications/dd.app`.
- All path deps (`dd-display`→`dd-term-core`,`dd-gpu`; `dd-gui`→`dd-client`,`dd-term-core`;
  `dd-gpu`→`dd-jit`; `dd-tests`→`dd-jit`) point at `../dd-*` dirs — update path + name + `use` idents.

### Fresh-cutover / on-disk (per decided scheme — no migration, but coordinate the whole tree)
`~/.dd` → `~/.husklet` touches dd-gpu, dd-gui, dd-term-core, the Makefile, and the cuda/nvml build
scripts (`~/.dd/c`, `~/.dd/n`) — plus the (out-of-scope) daemon that owns `~/.dd/run/docker.sock` and
`~/.dd/images`. Keep the `docker.sock` **filename** (Docker-API client compat); only the parent dir
moves. Config filenames (`term.conf`, `term-defaults.conf`, `workspaces.conf`) are not `dd`-branded —
KEEP.

### Naming collisions under the flat scheme
- `DDJIT_SANDBOX` (referenced dd-tests/cases/container.rs) and the engine's `DD_SANDBOX` both naively
  map to `HL_SANDBOX` — same collision flagged in the engine doc §1b; resolve there, mirror here.
- `DD_DMABUF_MOD_MAGIC`/`DD_DMABUF_RENDER_BIT` exist as **both** a Rust const (dd-display) and a C
  macro (engine) with identical names — the `HL_` rename must hit both files.
- `dd_gpu_msg_t` (dd-display mach bridge) is a distinct type from the engine's `dd_gpu_*` GPU registry
  fns (engine doc §2b) — same `hl_gpu_*` prefix, but different translation units; no real clash.

### Probably low-value renames
- Test-fixture tokens: `DD_ENV` (arbitrary printenv fixture), `DDEOF` heredoc marker, `/dd_ro_probe`
  test paths, bench CSV column labels — internal to the harness, cosmetic.
- `DD_DISPLAY_DEBUG`/`DD_DISPLAY_DUMP`/`DD_IR_DUMP` — dev-only display debug gates.

---

## NOT covered by this pass (for a later pass)
- Crates `dd-cli`, `dd-daemon`, `dd-client`, `dd-images` (their internal `dd_*` symbols/paths/strings);
  only their env/name overlap with this scope is flagged above.
- The `ddockerd` binary rename decision (drives `DDOCKERD_SOCK`) and the `ddcli` binary rename.
- `dd-jit` / `dd-jit-darwin` — see `dd-jit-inventory.md`.
- `website/blog/` and root `README.md` bulk brand copy (flagged, not enumerated line-by-line).
